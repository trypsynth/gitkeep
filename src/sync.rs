use std::{
	collections::{HashMap, HashSet},
	fmt::Write as _,
	fs,
	io::{self, Write as _},
	path::Path,
	process::{Command, Stdio},
	string::ToString,
};

use anyhow::{Context, Error, Result, bail};
use chrono::{DateTime, Utc};

use crate::{
	config::{Config, State, TrackedAccount},
	forge::{Forge, RemoteRepo, archive_path, split_host},
	utils::plural,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
	Quiet,
	Normal,
	Verbose,
}

#[derive(Clone, Copy, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct SyncOptions {
	pub force_forks: bool,
	pub force_submodules: bool,
	pub pull_only: bool,
	pub new_only: bool,
}

#[derive(Default)]
struct Totals {
	pulled_updated: usize,
	pulled_up_to_date: usize,
	cloned: usize,
	excluded: usize,
	failed: usize,
	updated_repos: Vec<String>,
	new_repos: Vec<String>,
}

/// Read-only settings shared by every repo synced during a single run.
#[derive(Clone, Copy)]
struct SyncContext<'a> {
	archive_dir: &'a Path,
	opts: SyncOptions,
	verbosity: Verbosity,
}

/// The mutable sync state and running totals threaded through a single run.
struct SyncState<'a> {
	state: &'a mut State,
	totals: &'a mut Totals,
}

/// Identifying details for a single repo being cloned or pulled.
struct RepoInfo<'a> {
	full_name: &'a str,
	id: u64,
	pushed_at: Option<DateTime<Utc>>,
}

pub async fn run(extra_users: &[String], opts: SyncOptions, verbosity: Verbosity) -> Result<()> {
	let mut config = Config::load().context("Could not load config")?;
	let mut updated = false;
	for user in extra_users {
		if let Some((host, path)) = split_host(user) {
			if !config.track.iter().any(|u| u.host.as_deref() == Some(host) && u.name.eq_ignore_ascii_case(path)) {
				bail!("'{user}' is not tracked yet. Use 'gitkeep add https://{user}' to start tracking it.");
			}
		} else if config.add_account(None, user, false, false, None) {
			updated = true;
		}
	}
	if updated {
		config.save().context("Could not update config")?;
	}
	if config.track.is_empty() && config.pinned.is_empty() {
		bail!(
			"Nothing to sync. Use 'gitkeep add <username>' to start building your library, \
             or run 'gitkeep login' to authenticate and auto-add your account."
		);
	}
	let to_sync: Vec<TrackedAccount> = config.track.iter().filter(|u| !u.frozen).cloned().collect();
	if to_sync.is_empty() && config.pinned.is_empty() {
		println!("All tracked users are frozen. Use 'gitkeep sync <username>' to sync specific accounts.");
		return Ok(());
	}
	// Include pinned repos whose owner is not covered by a tracked user.
	let pinned_to_sync: Vec<String> =
		config.pinned.iter().map(|p| &p.full_name).filter(|p| !to_sync.iter().any(|t| t.covers(p))).cloned().collect();
	sync_all(&mut config, &to_sync, &pinned_to_sync, opts, verbosity).await
}

pub async fn run_for(targets: &[String], opts: SyncOptions) -> Result<()> {
	let mut config = Config::load().context("Could not load config")?;
	let to_sync: Vec<TrackedAccount> =
		config.track.iter().filter(|u| targets.iter().any(|t| matches_target(u, t))).cloned().collect();
	if to_sync.is_empty() {
		println!("No matching users found to sync.");
		return Ok(());
	}
	sync_all(&mut config, &to_sync, &[], opts, Verbosity::Normal).await
}

/// Syncs a specific set of pinned repos (used right after `gitkeep add user/repo`).
pub async fn run_pinned(repos: &[String]) -> Result<()> {
	if repos.is_empty() {
		return Ok(());
	}
	let mut config = Config::load().context("Could not load config")?;
	sync_all(&mut config, &[], repos, SyncOptions::default(), Verbosity::Normal).await
}

async fn sync_all(
	config: &mut Config,
	users: &[TrackedAccount],
	pinned: &[String],
	opts: SyncOptions,
	verbosity: Verbosity,
) -> Result<()> {
	let archive_dir = config.archive_dir()?;
	fs::create_dir_all(&archive_dir)
		.with_context(|| format!("Could not create archive directory: {}", archive_dir.display()))?;
	let mut state = State::load()?;
	let legacy = state.drain_legacy_skipped();
	if !legacy.is_empty() {
		config.excluded.extend(legacy);
	}
	let mut totals = Totals::default();
	let ctx = SyncContext { archive_dir: &archive_dir, opts, verbosity };
	let mut sync_state = SyncState { state: &mut state, totals: &mut totals };
	let mut forges: HashMap<Option<String>, Result<Forge>> = HashMap::new();
	let mut seen = HashSet::new();
	let mut config_changed = false;
	for user in users {
		if !seen.insert(user.display_name()) {
			continue;
		}
		let forge = forges.entry(user.host.clone()).or_insert_with(|| config.forge(user.host.as_deref()));
		match forge {
			Ok(forge) => config_changed |= sync_one(user, forge, ctx, config, &mut sync_state).await,
			Err(e) => {
				eprintln!("  Could not sync {}: {e:#}.", user.display_name());
				sync_state.totals.failed += 1;
			}
		}
	}
	for full_name in pinned {
		let host = split_host(full_name).map(|(h, _)| h.to_string());
		let forge = forges.entry(host.clone()).or_insert_with(|| config.forge(host.as_deref()));
		match forge {
			Ok(forge) => config_changed |= sync_one_pinned(full_name, forge, ctx, config, &mut sync_state).await,
			Err(e) => {
				eprintln!("  Could not sync {full_name}: {e:#}.");
				sync_state.totals.failed += 1;
			}
		}
	}
	state.save().context("Could not save sync state")?;
	if config_changed {
		config.save().context("Could not save config after correcting username casing")?;
	}
	if verbosity == Verbosity::Normal
		&& let Some(detail) = build_normal_detail(&totals)
	{
		println!("{detail}");
		println!();
	}
	println!("{}", build_summary(&totals));
	Ok(())
}

fn build_summary(totals: &Totals) -> String {
	let total_processed = totals.pulled_updated + totals.pulled_up_to_date + totals.cloned + totals.failed;
	if total_processed == 0 {
		return if totals.excluded > 0 { "Done.".to_string() } else { "Nothing to do.".to_string() };
	}
	let mut parts: Vec<String> = Vec::new();
	if totals.cloned > 0 {
		parts.push(format!("{} cloned", plural(totals.cloned, "new repo", "new repos")));
	}
	if totals.pulled_updated > 0 {
		parts.push(format!("{} with new commits", plural(totals.pulled_updated, "repo", "repos")));
	}
	if totals.pulled_up_to_date > 0 {
		parts.push(format!("{} up to date", plural(totals.pulled_up_to_date, "repo", "repos")));
	}
	if totals.failed > 0 {
		parts.push(format!("{} failed", plural(totals.failed, "repo", "repos")));
	}
	format!("Done. {}.", parts.join(", "))
}

fn build_normal_detail(totals: &Totals) -> Option<String> {
	if totals.new_repos.is_empty() && totals.updated_repos.is_empty() {
		return None;
	}
	let mut out = String::new();
	if !totals.new_repos.is_empty() {
		out.push_str("Cloned:\n");
		for r in &totals.new_repos {
			let _ = writeln!(out, "  {r}");
		}
	}
	if !totals.updated_repos.is_empty() {
		if !out.is_empty() {
			out.push('\n');
		}
		out.push_str("Updated:\n");
		for r in &totals.updated_repos {
			let _ = writeln!(out, "  {r}");
		}
	}
	Some(out.trim_end().to_string())
}

/// True when a `sync <target>` argument names this tracked account: a plain name matches
/// a GitHub account, a `host/path` name matches an account on that host.
fn matches_target(user: &TrackedAccount, target: &str) -> bool {
	if let Some((host, path)) = split_host(target) {
		user.host.as_deref() == Some(host) && user.name.eq_ignore_ascii_case(path)
	} else {
		user.host.is_none() && user.name.eq_ignore_ascii_case(target)
	}
}

/// Syncs every repo of one tracked account. Follows account renames (by stable id, where the forge
/// has one), moving the local archive and updating the config. Returns `true` if the config changed.
async fn sync_one(
	user: &TrackedAccount,
	forge: &Forge,
	ctx: SyncContext<'_>,
	config: &mut Config,
	sync_state: &mut SyncState<'_>,
) -> bool {
	let display = user.display_name();
	if ctx.verbosity == Verbosity::Verbose {
		println!("Checking {display}...");
	}
	let account = match forge.account(&user.name, user.id).await {
		Ok(account) => account,
		Err(e) => {
			eprintln!("  Could not fetch repositories for {display}: {e:#}.");
			sync_state.totals.failed += 1;
			return false;
		}
	};
	let mut config_changed = false;
	let current = if let Some(entry) =
		config.track.iter_mut().find(|u| u.host == user.host && u.name.eq_ignore_ascii_case(&user.name))
	{
		if account.id.is_some() && entry.id != account.id {
			entry.id = account.id;
			config_changed = true;
		}
		if account.name != entry.name {
			let old_dir = archive_path(ctx.archive_dir, &entry.display_name());
			entry.name.clone_from(&account.name);
			let new_dir = archive_path(ctx.archive_dir, &entry.display_name());
			if old_dir.exists()
				&& !new_dir.exists()
				&& let Err(e) = fs::rename(&old_dir, &new_dir)
			{
				eprintln!("  Could not rename {} to {}: {e}.", old_dir.display(), new_dir.display());
			}
			if ctx.verbosity != Verbosity::Quiet {
				println!("Username updated: {display} → {}", entry.display_name());
			}
			config_changed = true;
		}
		entry.clone()
	} else {
		user.clone()
	};
	let include_forks = ctx.opts.force_forks || user.forks;
	let use_submodules = resolve_submodules(ctx.opts.force_submodules, user.submodules, config.submodules);
	let add_hint =
		current.host.as_ref().map_or_else(|| current.name.clone(), |h| format!("https://{h}/{}", current.name));
	report_found(&account.repos, &current.display_name(), &add_hint, include_forks, ctx.verbosity);
	sync_repo_list(account.repos, include_forks, use_submodules, ctx, config, sync_state);
	config_changed
}

/// Prints the verbose "Found N repositories" line, with a fork-skip hint when relevant.
fn report_found(repos: &[RemoteRepo], display: &str, add_target: &str, include_forks: bool, verbosity: Verbosity) {
	if verbosity != Verbosity::Verbose {
		return;
	}
	let fork_count = repos.iter().filter(|r| r.fork).count();
	let visible = repos.len() - if include_forks { 0 } else { fork_count };
	let mut msg = format!("Found {} for {}.", plural(visible, "repository", "repositories"), display);
	if !include_forks && fork_count > 0 {
		let _ = write!(
			msg,
			" Skipping {}. Use 'gitkeep add --forks {}' to include them.",
			plural(fork_count, "fork", "forks"),
			add_target
		);
	}
	println!("{msg}");
}

/// Syncs one individually pinned repo. Follows renames (by stable id, where the forge has one),
/// moving the local copy and updating the pin. Returns `true` if the config changed.
async fn sync_one_pinned(
	full_name: &str,
	forge: &Forge,
	ctx: SyncContext<'_>,
	config: &mut Config,
	sync_state: &mut SyncState<'_>,
) -> bool {
	if ctx.verbosity == Verbosity::Verbose {
		println!("Checking {full_name}...");
	}
	let path = split_host(full_name).map_or(full_name, |(_, path)| path);
	let stored_id = config.pinned_id(full_name);
	let use_submodules =
		resolve_submodules(ctx.opts.force_submodules, config.pinned_submodules(full_name), config.submodules);
	let repo = match forge.repo(path, stored_id).await {
		Ok(Some(repo)) => repo,
		Ok(None) => {
			eprintln!("  Could not fetch {full_name}.");
			sync_state.totals.failed += 1;
			return false;
		}
		Err(e) => {
			eprintln!("  Could not fetch {full_name}: {e:#}.");
			sync_state.totals.failed += 1;
			return false;
		}
	};
	let mut config_changed = repo.full_name != full_name;
	if config_changed {
		let old_dir = archive_path(ctx.archive_dir, full_name);
		let new_dir = archive_path(ctx.archive_dir, &repo.rel_dir);
		if old_dir.exists()
			&& !new_dir.exists()
			&& let Some(parent) = new_dir.parent()
			&& let Err(e) = fs::create_dir_all(parent).and_then(|()| fs::rename(&old_dir, &new_dir))
		{
			eprintln!("  Could not rename {} to {}: {e}.", old_dir.display(), new_dir.display());
		}
		if ctx.verbosity != Verbosity::Quiet {
			println!("Pinned repo updated: {full_name} → {}", repo.full_name);
		}
		config.rename_pin(full_name, &repo.full_name);
	}
	if stored_id != Some(repo.id)
		&& let Some(pin) = config.pinned.iter_mut().find(|p| p.full_name == repo.full_name)
	{
		pin.id = Some(repo.id);
		config_changed = true;
	}
	sync_repo_list(vec![repo], true, use_submodules, ctx, config, sync_state);
	config_changed
}

fn sync_repo_list(
	repos: Vec<RemoteRepo>,
	include_forks: bool,
	use_submodules: bool,
	ctx: SyncContext<'_>,
	config: &Config,
	sync_state: &mut SyncState<'_>,
) {
	for repo in repos {
		let full_name = repo.full_name.as_str();
		if config.is_excluded(full_name) {
			sync_state.totals.excluded += 1;
			continue;
		}
		if repo.fork && !include_forks {
			sync_state.totals.excluded += 1;
			continue;
		}
		let Some(url) = repo.clone_url(config.use_ssh) else {
			sync_state.totals.excluded += 1;
			continue;
		};
		let mut repo_dir = ctx.archive_dir.to_path_buf();
		repo_dir.extend(repo.rel_dir.split('/'));
		let already_cloned = repo_dir.exists();
		if already_cloned && ctx.opts.new_only {
			sync_state.totals.excluded += 1;
			continue;
		}
		if !already_cloned && ctx.opts.pull_only {
			sync_state.totals.excluded += 1;
			continue;
		}
		let info = RepoInfo { full_name, id: repo.id, pushed_at: repo.pushed_at };
		if already_cloned {
			pull_and_record(&repo_dir, &url, use_submodules, ctx.verbosity, &info, sync_state);
		} else {
			if let Some(parent) = repo_dir.parent()
				&& let Err(e) = fs::create_dir_all(parent)
			{
				eprintln!("  Could not create directory for {full_name}: {e}.");
				sync_state.totals.failed += 1;
				continue;
			}
			if ctx.verbosity == Verbosity::Verbose {
				println!("Cloning {full_name}...");
			}
			clone_and_record(&url, &repo_dir, use_submodules, ctx.verbosity, "clone", &info, sync_state);
		}
	}
}

fn pull_and_record(
	repo_dir: &Path,
	url: &str,
	use_submodules: bool,
	verbosity: Verbosity,
	info: &RepoInfo,
	sync_state: &mut SyncState<'_>,
) {
	let stored = sync_state.state.repos.get(info.full_name);
	if let Some(stored_id) = stored.and_then(|s| s.id)
		&& stored_id != info.id
	{
		if verbosity != Verbosity::Quiet {
			println!("  {} was recreated as a different repository, re-cloning...", info.full_name);
		}
		reclone(url, repo_dir, use_submodules, verbosity, info, sync_state);
		return;
	}
	let state_pushed_at = stored.and_then(|s| s.pushed_at);
	if should_skip_pull(info.pushed_at, state_pushed_at) {
		sync_state.totals.pulled_up_to_date += 1;
		return;
	}
	if verbosity == Verbosity::Verbose {
		println!("Pulling {}...", info.full_name);
	}
	match git_pull(repo_dir, verbosity) {
		PullOutcome::Updated => {
			sync_state.state.mark_synced(info.full_name, info.pushed_at, info.id);
			if use_submodules && let Err(e) = update_submodules(repo_dir, verbosity) {
				eprintln!("  Could not update submodules for {}: {e:#}.", info.full_name);
			}
			if verbosity == Verbosity::Normal {
				sync_state.totals.updated_repos.push(info.full_name.to_string());
			}
			sync_state.totals.pulled_updated += 1;
		}
		PullOutcome::UpToDate => {
			sync_state.state.mark_synced(info.full_name, info.pushed_at, info.id);
			sync_state.totals.pulled_up_to_date += 1;
		}
		PullOutcome::Fatal => {
			if verbosity == Verbosity::Verbose {
				println!("  Pull failed for {}, re-cloning...", info.full_name);
			}
			reclone(url, repo_dir, use_submodules, verbosity, info, sync_state);
		}
		PullOutcome::Failed(e) => {
			eprintln!("  Failed to pull {}: {e:#}.", info.full_name);
			sync_state.totals.failed += 1;
		}
	}
}

/// Removes an existing local clone and clones it fresh, e.g. when the remote history is
/// gone (exit 128) or `owner/name` now points at an unrelated repo (stored id changed).
fn reclone(
	url: &str,
	repo_dir: &Path,
	use_submodules: bool,
	verbosity: Verbosity,
	info: &RepoInfo,
	sync_state: &mut SyncState<'_>,
) {
	if let Err(e) = fs::remove_dir_all(repo_dir) {
		eprintln!("  Could not remove {}: {e}.", repo_dir.display());
		sync_state.totals.failed += 1;
		return;
	}
	clone_and_record(url, repo_dir, use_submodules, verbosity, "re-clone", info, sync_state);
}

fn clone_and_record(
	url: &str,
	repo_dir: &Path,
	use_submodules: bool,
	verbosity: Verbosity,
	action: &str,
	info: &RepoInfo,
	sync_state: &mut SyncState<'_>,
) {
	match git_clone(url, repo_dir, verbosity) {
		Ok(()) => {
			sync_state.state.mark_synced(info.full_name, info.pushed_at, info.id);
			if use_submodules && let Err(e) = update_submodules(repo_dir, verbosity) {
				eprintln!("  Could not clone submodules for {}: {e:#}.", info.full_name);
			}
			if verbosity == Verbosity::Normal {
				sync_state.totals.new_repos.push(info.full_name.to_string());
			}
			sync_state.totals.cloned += 1;
		}
		Err(e) => {
			eprintln!("  Failed to {action} {}: {e:#}.", info.full_name);
			sync_state.totals.failed += 1;
		}
	}
}

fn should_skip_pull(repo_pushed_at: Option<DateTime<Utc>>, state_pushed_at: Option<DateTime<Utc>>) -> bool {
	match (repo_pushed_at, state_pushed_at) {
		(Some(repo), Some(state)) => repo <= state,
		_ => false,
	}
}

/// Resolves whether submodules should be cloned/updated, in priority order: an explicit
/// one-off `--submodules` flag, then a per-account/per-pin override, then the global default.
fn resolve_submodules(force: bool, override_: Option<bool>, global_default: bool) -> bool {
	force || override_.unwrap_or(global_default)
}

fn git_head(repo_dir: &Path) -> Option<String> {
	let out = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(repo_dir).output().ok()?;
	if out.status.success() { Some(String::from_utf8_lossy(&out.stdout).trim().to_string()) } else { None }
}

enum PullOutcome {
	Updated,
	UpToDate,
	Fatal,
	Failed(Error),
}

fn git_pull(repo_dir: &Path, verbosity: Verbosity) -> PullOutcome {
	let head_before = git_head(repo_dir);
	let output = match Command::new("git").arg("pull").current_dir(repo_dir).output() {
		Ok(out) => out,
		Err(e) => {
			return PullOutcome::Failed(
				Error::from(e).context("Could not run 'git pull'. Is git installed and on your PATH?"),
			);
		}
	};
	if verbosity == Verbosity::Verbose {
		io::stdout().write_all(&output.stdout).ok();
		io::stderr().write_all(&output.stderr).ok();
	}
	let exit_code = output.status.code().unwrap_or(-1);
	if exit_code == 0 {
		let head_after = git_head(repo_dir);
		if head_before == head_after { PullOutcome::UpToDate } else { PullOutcome::Updated }
	} else if exit_code == 128 || indicates_repo_identity_mismatch(&output.stderr) {
		PullOutcome::Fatal
	} else {
		PullOutcome::Failed(anyhow::anyhow!("git pull failed with code {exit_code}."))
	}
}

/// True when a failed `git pull`'s stderr shows the local checkout no longer matches what's on
/// the remote — e.g. `owner/name` was deleted and recreated as an unrelated repo, so the branch
/// it used to track is gone or the histories share no common ancestor. Distinguishes that
/// permanent case (which should trigger a re-clone) from transient failures like a network or
/// auth error (which should just be reported).
fn indicates_repo_identity_mismatch(stderr: &[u8]) -> bool {
	let stderr = String::from_utf8_lossy(stderr);
	stderr.contains("unrelated histories") || stderr.contains("but no such ref was fetched")
}

/// Runs a git subcommand, streaming its output to the terminal in verbose mode and suppressing
/// it otherwise. `action` names the command in error messages (e.g. `"git clone"`).
fn run_git(mut cmd: Command, verbosity: Verbosity, action: &str) -> Result<()> {
	let status = if verbosity == Verbosity::Verbose {
		cmd.status()
	} else {
		cmd.stdout(Stdio::null()).stderr(Stdio::null()).status()
	}
	.with_context(|| format!("Could not run '{action}'. Is git installed and on your PATH?"))?;
	if !status.success() {
		bail!("{action} failed with code {}.", status.code().unwrap_or(-1));
	}
	Ok(())
}

fn git_clone(url: &str, dest: &Path, verbosity: Verbosity) -> Result<()> {
	let mut cmd = Command::new("git");
	cmd.args(["clone", "--", url]).arg(dest);
	run_git(cmd, verbosity, "git clone")
}

/// Initializes and updates submodules to the commit recorded by the superproject. Idempotent,
/// and a no-op if the repo has no `.gitmodules`, so it's safe to call after every clone/pull.
fn update_submodules(repo_dir: &Path, verbosity: Verbosity) -> Result<()> {
	if !repo_dir.join(".gitmodules").exists() {
		return Ok(());
	}
	let mut cmd = Command::new("git");
	cmd.args(["submodule", "update", "--init", "--recursive"]).current_dir(repo_dir);
	run_git(cmd, verbosity, "git submodule update")
}

#[cfg(test)]
mod tests {
	use chrono::Duration;

	use super::*;

	#[test]
	fn skip_pull_when_pushed_at_matches() {
		let t = Utc::now();
		assert!(should_skip_pull(Some(t), Some(t)));
	}

	#[test]
	fn skip_pull_when_repo_older_than_state() {
		let older = Utc::now() - Duration::hours(1);
		let newer = Utc::now();
		assert!(should_skip_pull(Some(older), Some(newer)));
	}

	#[test]
	fn pull_when_repo_pushed_at_is_newer() {
		let older = Utc::now() - Duration::hours(1);
		let newer = Utc::now();
		assert!(!should_skip_pull(Some(newer), Some(older)));
	}

	#[test]
	fn pull_when_no_state_pushed_at() {
		assert!(!should_skip_pull(Some(Utc::now()), None));
	}

	#[test]
	fn pull_when_no_repo_pushed_at() {
		assert!(!should_skip_pull(None, Some(Utc::now())));
	}

	#[test]
	fn resolve_submodules_uses_global_default_when_no_override_or_force() {
		assert!(resolve_submodules(false, None, true));
		assert!(!resolve_submodules(false, None, false));
	}

	#[test]
	fn resolve_submodules_override_beats_global_default() {
		assert!(resolve_submodules(false, Some(true), false));
		assert!(!resolve_submodules(false, Some(false), true));
	}

	#[test]
	fn resolve_submodules_force_beats_everything() {
		assert!(resolve_submodules(true, Some(false), false));
	}

	#[test]
	fn pull_when_neither_pushed_at() {
		assert!(!should_skip_pull(None, None));
	}

	#[test]
	fn matches_target_plain_name_matches_github_only() {
		let github = TrackedAccount::with_options("alice", false, false);
		let gitlab = TrackedAccount {
			host: Some("gitlab.example.com".to_string()),
			..TrackedAccount::with_options("alice", false, false)
		};
		assert!(matches_target(&github, "Alice"));
		assert!(!matches_target(&gitlab, "Alice"));
	}

	#[test]
	fn matches_target_host_qualified_matches_gitlab_entry() {
		let gitlab = TrackedAccount {
			host: Some("gitlab.example.com".to_string()),
			..TrackedAccount::with_options("grp/sub", false, false)
		};
		assert!(matches_target(&gitlab, "gitlab.example.com/grp/sub"));
		assert!(!matches_target(&gitlab, "gitlab.other.com/grp/sub"));
	}

	#[test]
	fn identity_mismatch_detects_unrelated_histories() {
		assert!(indicates_repo_identity_mismatch(b"fatal: refusing to merge unrelated histories"));
	}

	#[test]
	fn identity_mismatch_detects_missing_tracked_ref() {
		let stderr = b"Your configuration specifies to merge with the ref 'refs/heads/master'\n\
			from the remote, but no such ref was fetched.";
		assert!(indicates_repo_identity_mismatch(stderr));
	}

	#[test]
	fn identity_mismatch_false_for_transient_failure() {
		assert!(!indicates_repo_identity_mismatch(
			b"fatal: unable to access 'https://github.com/x/y.git': Could not resolve host"
		));
	}

	#[test]
	fn summary_nothing_to_do_when_truly_empty() {
		let s = build_summary(&Totals::default());
		assert_eq!(s, "Nothing to do.");
	}

	#[test]
	fn summary_does_not_show_excluded() {
		let s = build_summary(&Totals { excluded: 5, ..Totals::default() });
		assert!(!s.contains("excluded"), "got: {s}");
		assert!(!s.contains("skipped"), "got: {s}");
	}

	#[test]
	fn summary_shows_done_when_only_excluded() {
		let s = build_summary(&Totals { excluded: 5, ..Totals::default() });
		assert_eq!(s, "Done.");
	}

	#[test]
	fn detail_empty_when_nothing_notable() {
		let totals = Totals { pulled_up_to_date: 5, ..Totals::default() };
		assert!(build_normal_detail(&totals).is_none());
	}

	#[test]
	fn detail_shows_cloned_section() {
		let totals = Totals { new_repos: vec!["alice/fresh".to_string()], cloned: 1, ..Totals::default() };
		let detail = build_normal_detail(&totals).unwrap();
		assert!(detail.contains("Cloned"), "got: {detail}");
		assert!(detail.contains("alice/fresh"), "got: {detail}");
	}

	#[test]
	fn detail_shows_updated_section() {
		let totals = Totals { updated_repos: vec!["alice/old".to_string()], pulled_updated: 1, ..Totals::default() };
		let detail = build_normal_detail(&totals).unwrap();
		assert!(detail.contains("Updated"), "got: {detail}");
		assert!(detail.contains("alice/old"), "got: {detail}");
	}

	#[test]
	fn detail_omits_empty_sections() {
		let totals = Totals { updated_repos: vec!["alice/repo".to_string()], pulled_updated: 1, ..Totals::default() };
		let detail = build_normal_detail(&totals).unwrap();
		assert!(!detail.contains("Cloned"), "got: {detail}");
	}

	#[test]
	fn detail_shows_both_sections_when_populated() {
		let totals = Totals {
			new_repos: vec!["alice/new".to_string()],
			updated_repos: vec!["alice/old".to_string()],
			cloned: 1,
			pulled_updated: 1,
			..Totals::default()
		};
		let detail = build_normal_detail(&totals).unwrap();
		assert!(detail.contains("Cloned"), "got: {detail}");
		assert!(detail.contains("Updated"), "got: {detail}");
	}
}
