use std::{
	collections::{HashMap, HashSet},
	fmt::Write as _,
	fs,
	path::Path,
	string::ToString,
};

use anyhow::{Context, Result, bail};

mod git;
mod repo;
mod summary;

use self::{
	repo::sync_repo_list,
	summary::{Totals, build_normal_detail, build_summary},
};
use crate::{
	config::{Config, State, TrackedAccount},
	forge::{Forge, RemoteRepo, Target, archive_path, split_host},
	track,
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

/// Syncs everything tracked, or with `targets`, just those accounts and repos (frozen or not).
/// Targets that aren't tracked yet are added first, exactly as `gitkeep add` would.
pub async fn run(targets: &[String], opts: SyncOptions, verbosity: Verbosity) -> Result<()> {
	if targets.is_empty() {
		return run_all(opts, verbosity).await;
	}
	let config = Config::load().context("Could not load config")?;
	let untracked: Vec<String> =
		targets.iter().filter(|t| matches!(classify(&config, t), Named::Untracked)).cloned().collect();
	if !untracked.is_empty() {
		track::add(&untracked, false, false, None).await?;
	}
	let mut config = Config::load().context("Could not load config")?;
	let mut accounts: Vec<TrackedAccount> = Vec::new();
	let mut pinned: Vec<String> = Vec::new();
	for target in targets {
		match classify(&config, target) {
			Named::Account(account) => accounts.push(account),
			Named::Pin(pin) if !pinned.contains(&pin) => pinned.push(pin),
			Named::Pin(_) | Named::Untracked => {}
		}
	}
	if accounts.is_empty() && pinned.is_empty() {
		println!("Nothing to sync.");
		return Ok(());
	}
	sync_all(&mut config, &accounts, &pinned, opts, verbosity).await
}

async fn run_all(opts: SyncOptions, verbosity: Verbosity) -> Result<()> {
	let mut config = Config::load().context("Could not load config")?;
	if config.track.is_empty() && config.pinned.is_empty() {
		bail!(
			"Nothing to sync. Use 'gitkeep add <username>' to start building your library, \
             or run 'gitkeep login' to authenticate and auto-add your account."
		);
	}
	let to_sync: Vec<TrackedAccount> = config.track.iter().filter(|u| !u.frozen).cloned().collect();
	if to_sync.is_empty() && config.pinned.is_empty() {
		println!("All tracked accounts are frozen. Use 'gitkeep sync <account>' to sync one anyway.");
		return Ok(());
	}
	let pinned_to_sync: Vec<String> =
		config.pinned.iter().map(|p| &p.full_name).filter(|p| !to_sync.iter().any(|t| t.covers(p))).cloned().collect();
	sync_all(&mut config, &to_sync, &pinned_to_sync, opts, verbosity).await
}

/// What a `sync <target>` argument refers to in the config.
enum Named {
	Account(TrackedAccount),
	Pin(String),
	Untracked,
}

/// Matches a `sync` argument against what's tracked. A repo inside a tracked account means that
/// account, unless the repo was removed from it.
fn classify(config: &Config, arg: &str) -> Named {
	let key = Target::parse(arg).key();
	if let Some(account) = config.track.iter().find(|u| matches_target(u, &key)) {
		return Named::Account(account.clone());
	}
	if let Some(pin) = config.pinned.iter().find(|p| p.full_name.eq_ignore_ascii_case(&key)) {
		return Named::Pin(pin.full_name.clone());
	}
	match config.track.iter().find(|u| u.covers(&key)) {
		Some(owner) if !config.is_excluded(&key) => Named::Account(owner.clone()),
		_ => Named::Untracked,
	}
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

/// Resolves whether submodules should be cloned/updated, in priority order: an explicit
/// one-off `--submodules` flag, then a per-account/per-pin override, then the global default.
fn resolve_submodules(force: bool, override_: Option<bool>, global_default: bool) -> bool {
	force || override_.unwrap_or(global_default)
}

#[cfg(test)]
mod tests {
	use super::*;

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

	fn named(config: &Config, arg: &str) -> String {
		match classify(config, arg) {
			Named::Account(a) => format!("account {}", a.display_name()),
			Named::Pin(p) => format!("pin {p}"),
			Named::Untracked => "untracked".to_string(),
		}
	}

	#[test]
	fn classify_matches_frozen_accounts_too() {
		let mut config = Config::default();
		config.add_account(None, "Alice", false, true, None);
		assert_eq!(named(&config, "alice"), "account Alice");
	}

	#[test]
	fn classify_matches_pins_ignoring_case() {
		let mut config = Config::default();
		config.pin_repo_with_options("bob/Repo", None, None);
		assert_eq!(named(&config, "Bob/repo"), "pin bob/Repo");
	}

	#[test]
	fn classify_maps_repo_to_its_tracked_owner() {
		let mut config = Config::default();
		config.add_account(None, "alice", false, false, None);
		assert_eq!(named(&config, "alice/repo"), "account alice");
	}

	#[test]
	fn classify_treats_removed_repo_as_untracked() {
		let mut config = Config::default();
		config.add_account(None, "alice", false, false, None);
		config.exclude_repo("alice/big");
		assert_eq!(named(&config, "alice/big"), "untracked");
	}

	#[test]
	fn classify_distinguishes_hosts() {
		let mut config = Config::default();
		config.add_account(Some("gitlab.example.com"), "alice", false, false, None);
		assert_eq!(named(&config, "alice"), "untracked");
		assert_eq!(named(&config, "https://gitlab.example.com/alice"), "account gitlab.example.com/alice");
		assert_eq!(named(&config, "gitlab.example.com/alice/proj"), "account gitlab.example.com/alice");
	}

	#[test]
	fn classify_untracked_name() {
		assert_eq!(named(&Config::default(), "carol"), "untracked");
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
}
