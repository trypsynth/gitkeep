use std::{
	collections::HashSet,
	fmt::Write as _,
	fs,
	path::Path,
	process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use octocrab::{Octocrab, models::Repository};
use serde::Deserialize;

use crate::{
	config::{Config, State, TrackedUser},
	utils::plural,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
	Quiet,
	Normal,
	Verbose,
}

#[derive(Default)]
struct Totals {
	pulled_updated: usize,
	pulled_up_to_date: usize,
	cloned: usize,
	skipped: usize,
	ignored: usize,
	failed: usize,
	updated_repos: Vec<String>,
	new_repos: Vec<String>,
}

pub async fn run(
	extra_users: &[String],
	force_forks: bool,
	pull_only: bool,
	new_only: bool,
	verbosity: Verbosity,
) -> Result<()> {
	let mut config = Config::load().context("Could not load config")?;
	let mut updated = false;
	for user in extra_users {
		if config.add_user(user, false, false) {
			updated = true;
		}
	}
	if updated {
		config.save().context("Could not update config")?;
	}
	if config.track.is_empty() {
		bail!(
			"Nothing to sync. Use 'gitkeep add <username>' to start building your library, \
             or run 'gitkeep login' to authenticate and auto-add your account."
		);
	}
	let to_sync: Vec<TrackedUser> = config.track.iter().filter(|u| !u.frozen).cloned().collect();
	if to_sync.is_empty() {
		println!("All tracked users are frozen. Use 'gitkeep sync <username>' to sync specific accounts.");
		return Ok(());
	}
	sync_all(&mut config, &to_sync, force_forks, pull_only, new_only, verbosity).await
}

pub async fn run_for(targets: &[String], force_forks: bool) -> Result<()> {
	let mut config = Config::load().context("Could not load config")?;
	let to_sync: Vec<TrackedUser> =
		config.track.iter().filter(|u| targets.iter().any(|t| t.eq_ignore_ascii_case(&u.name))).cloned().collect();
	if to_sync.is_empty() {
		println!("No matching users found to sync.");
		return Ok(());
	}
	sync_all(&mut config, &to_sync, force_forks, false, false, Verbosity::Normal).await
}

async fn sync_all(
	config: &mut Config,
	users: &[TrackedUser],
	force_forks: bool,
	pull_only: bool,
	new_only: bool,
	verbosity: Verbosity,
) -> Result<()> {
	let client = config.build_client()?;
	let archive_dir = config.archive_dir()?;
	fs::create_dir_all(&archive_dir)
		.with_context(|| format!("Could not create archive directory: {}", archive_dir.display()))?;
	let mut state = State::load()?;
	let legacy = state.drain_legacy_skipped();
	if !legacy.is_empty() {
		config.skipped.extend(legacy);
	}
	let mut totals = Totals::default();
	let mut seen = HashSet::new();
	let mut config_changed = false;
	for user in users {
		if seen.insert(user.name.as_str()) {
			let renamed = sync_one(
				user,
				force_forks,
				pull_only,
				new_only,
				verbosity,
				&client,
				&archive_dir,
				config,
				&mut state,
				&mut totals,
			)
			.await;
			if renamed {
				config_changed = true;
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
	let total_processed = totals.pulled_updated
		+ totals.pulled_up_to_date
		+ totals.cloned
		+ totals.failed
		+ totals.skipped
		+ totals.ignored;
	if total_processed == 0 {
		return "Nothing to do.".to_string();
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
	if totals.skipped > 0 {
		parts.push(format!("{} skipped", plural(totals.skipped, "repo", "repos")));
	}
	if totals.ignored > 0 {
		parts.push(format!("{} ignored", plural(totals.ignored, "repo", "repos")));
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

#[cfg(test)]
mod tests {
	use super::*;

	fn totals(ignored: usize) -> Totals {
		Totals { ignored, ..Totals::default() }
	}

	#[test]
	fn summary_shows_ignored_count() {
		let s = build_summary(&totals(2));
		assert!(s.contains("2 repos ignored"), "got: {s}");
	}

	#[test]
	fn summary_not_nothing_to_do_when_only_ignored() {
		let s = build_summary(&totals(1));
		assert_ne!(s, "Nothing to do.");
	}

	#[test]
	fn summary_nothing_to_do_when_truly_empty() {
		let s = build_summary(&Totals::default());
		assert_eq!(s, "Nothing to do.");
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

#[allow(clippy::too_many_arguments)]
async fn sync_one(
	user: &TrackedUser,
	force_forks: bool,
	pull_only: bool,
	new_only: bool,
	verbosity: Verbosity,
	client: &Octocrab,
	archive_dir: &Path,
	config: &mut Config,
	state: &mut State,
	totals: &mut Totals,
) -> bool {
	if verbosity == Verbosity::Verbose {
		println!("Checking {}...", user.name);
	}
	let account = fetch_account(client, &user.name).await;
	let canonical = account.as_ref().map_or_else(|| user.name.clone(), |a| a.login.clone());
	let is_org = account.is_some_and(|a| a.account_type == "Organization");

	let mut config_changed = false;
	if canonical != user.name {
		let old_dir = archive_dir.join(&user.name);
		let new_dir = archive_dir.join(&canonical);
		if old_dir.exists()
			&& !new_dir.exists()
			&& let Err(e) = fs::rename(&old_dir, &new_dir)
		{
			eprintln!("  Could not rename {} to {}: {e}.", old_dir.display(), new_dir.display());
		}
		if let Some(entry) = config.track.iter_mut().find(|u| u.name.eq_ignore_ascii_case(&user.name)) {
			if verbosity != Verbosity::Quiet {
				println!("Corrected username casing: {} → {}", entry.name, canonical);
			}
			entry.name.clone_from(&canonical);
			config_changed = true;
		}
	}

	let repos_result = if is_org {
		if config.token.is_some() {
			fetch_org(client, &canonical).await
		} else {
			fetch_public(client, &canonical).await
		}
	} else if config.token.is_some() {
		fetch_with_token(client, &canonical).await
	} else {
		fetch_public(client, &canonical).await
	};
	match repos_result {
		Ok(repos) => {
			let include_forks = force_forks || user.forks;
			let fork_count = repos.iter().filter(|r| r.fork.unwrap_or(false)).count();
			let visible = repos.len() - if include_forks { 0 } else { fork_count };
			if verbosity == Verbosity::Verbose {
				let mut msg = format!("Found {} for {}.", plural(visible, "repository", "repositories"), canonical);
				if !include_forks && fork_count > 0 {
					let _ = write!(
						msg,
						" Skipping {}. Use 'gitkeep add --forks {}' to include them.",
						plural(fork_count, "fork", "forks"),
						canonical
					);
				}
				println!("{msg}");
			}
			sync_repo_list(
				repos,
				&canonical,
				include_forks,
				pull_only,
				new_only,
				verbosity,
				archive_dir,
				config,
				state,
				totals,
			);
		}
		Err(e) => {
			eprintln!("  Could not fetch repositories for {canonical}: {e:#}.");
			totals.failed += 1;
		}
	}
	config_changed
}

#[allow(clippy::too_many_arguments)]
fn sync_repo_list(
	repos: Vec<Repository>,
	username: &str,
	include_forks: bool,
	pull_only: bool,
	new_only: bool,
	verbosity: Verbosity,
	archive_dir: &Path,
	config: &Config,
	state: &mut State,
	totals: &mut Totals,
) {
	let user_dir = archive_dir.join(username);
	if let Err(e) = fs::create_dir_all(&user_dir) {
		eprintln!("  Could not create directory for {username}: {e}.");
		totals.failed += repos.len();
		return;
	}
	for repo in repos {
		let name = &repo.name;
		let full_name = repo.full_name.as_deref().unwrap_or(name.as_str());
		if config.is_skipped(full_name) {
			totals.ignored += 1;
			continue;
		}
		if repo.fork.unwrap_or(false) && !include_forks {
			totals.skipped += 1;
			continue;
		}
		let Some(url) = clone_url(&repo, config.use_ssh) else {
			totals.skipped += 1;
			continue;
		};
		let repo_dir = user_dir.join(name.as_str());
		let already_cloned = repo_dir.exists();
		if already_cloned && new_only {
			totals.skipped += 1;
			continue;
		}
		if !already_cloned && pull_only {
			totals.skipped += 1;
			continue;
		}
		if already_cloned {
			if verbosity == Verbosity::Verbose {
				println!("Pulling {username}/{name}...");
			}
			match git_pull(&repo_dir, verbosity) {
				PullOutcome::Updated => {
					state.mark_synced(full_name);
					if verbosity == Verbosity::Normal {
						totals.updated_repos.push(format!("{username}/{name}"));
					}
					totals.pulled_updated += 1;
				}
				PullOutcome::UpToDate => {
					state.mark_synced(full_name);
					totals.pulled_up_to_date += 1;
				}
				PullOutcome::Fatal => {
					if verbosity == Verbosity::Verbose {
						println!("  Pull failed for {username}/{name} (exit 128), re-cloning...");
					}
					if let Err(e) = fs::remove_dir_all(&repo_dir) {
						eprintln!("  Could not remove {}: {e}.", repo_dir.display());
						totals.failed += 1;
						continue;
					}
					match git_clone(&url, &repo_dir, verbosity) {
						Ok(()) => {
							state.mark_synced(full_name);
							if verbosity == Verbosity::Normal {
								totals.new_repos.push(format!("{username}/{name}"));
							}
							totals.cloned += 1;
						}
						Err(e) => {
							eprintln!("  Failed to re-clone {username}/{name}: {e:#}.");
							totals.failed += 1;
						}
					}
				}
				PullOutcome::Failed(e) => {
					eprintln!("  Failed to pull {username}/{name}: {e:#}.");
					totals.failed += 1;
				}
			}
		} else {
			if verbosity == Verbosity::Verbose {
				println!("Cloning {username}/{name}...");
			}
			match git_clone(&url, &repo_dir, verbosity) {
				Ok(()) => {
					state.mark_synced(full_name);
					if verbosity == Verbosity::Normal {
						totals.new_repos.push(format!("{username}/{name}"));
					}
					totals.cloned += 1;
				}
				Err(e) => {
					eprintln!("  Failed to clone {username}/{name}: {e:#}.");
					totals.failed += 1;
				}
			}
		}
	}
}

async fn fetch_with_token(client: &Octocrab, username: &str) -> Result<Vec<Repository>> {
	let (public, accessible) = tokio::try_join!(fetch_public(client, username), fetch_authenticated(client, username))?;
	let mut seen = HashSet::new();
	let mut merged = Vec::with_capacity(public.len() + accessible.len());
	for repo in public.into_iter().chain(accessible) {
		if seen.insert(repo.id) {
			merged.push(repo);
		}
	}
	Ok(merged)
}

async fn fetch_authenticated(client: &Octocrab, username: &str) -> Result<Vec<Repository>> {
	let page = client
		.current()
		.list_repos_for_authenticated_user()
		.per_page(100u8)
		.send()
		.await
		.context("Could not fetch your repositories from GitHub")?;
	let repos = client.all_pages(page).await.context("Could not retrieve all repository pages")?;
	Ok(repos.into_iter().filter(|r| r.owner.as_ref().is_some_and(|o| o.login.eq_ignore_ascii_case(username))).collect())
}

#[derive(Deserialize)]
struct AccountInfo {
	login: String,
	#[serde(rename = "type")]
	account_type: String,
}

async fn fetch_account(client: &Octocrab, name: &str) -> Option<AccountInfo> {
	client.get(format!("/users/{name}"), None::<&()>).await.ok()
}

pub async fn resolve_login(client: &Octocrab, name: &str) -> Result<String> {
	let info: AccountInfo = client
		.get(format!("/users/{name}"), None::<&()>)
		.await
		.with_context(|| format!("Could not find GitHub user '{name}'"))?;
	Ok(info.login)
}

async fn fetch_org(client: &Octocrab, org: &str) -> Result<Vec<Repository>> {
	let page = client
		.orgs(org)
		.list_repos()
		.per_page(100u8)
		.send()
		.await
		.with_context(|| format!("Could not fetch repositories for org {org}"))?;
	client.all_pages(page).await.with_context(|| format!("Could not retrieve all repository pages for org {org}"))
}

async fn fetch_public(client: &Octocrab, username: &str) -> Result<Vec<Repository>> {
	let page = client
		.users(username)
		.repos()
		.per_page(100u8)
		.send()
		.await
		.with_context(|| format!("Could not fetch public repositories for {username}"))?;
	client.all_pages(page).await.with_context(|| format!("Could not retrieve all repository pages for {username}"))
}

fn clone_url(repo: &Repository, use_ssh: bool) -> Option<String> {
	if use_ssh { repo.ssh_url.clone() } else { repo.clone_url.as_ref().map(std::string::ToString::to_string) }
}

fn git_head(repo_dir: &Path) -> Option<String> {
	let out = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(repo_dir).output().ok()?;
	if out.status.success() { Some(String::from_utf8_lossy(&out.stdout).trim().to_string()) } else { None }
}

enum PullOutcome {
	Updated,
	UpToDate,
	Fatal,
	Failed(anyhow::Error),
}

fn git_pull(repo_dir: &Path, verbosity: Verbosity) -> PullOutcome {
	let head_before = git_head(repo_dir);
	let exit_code = if verbosity == Verbosity::Verbose {
		match Command::new("git").arg("pull").current_dir(repo_dir).status() {
			Ok(s) => s.code().unwrap_or(-1),
			Err(e) => {
				return PullOutcome::Failed(
					anyhow::Error::from(e).context("Could not run 'git pull'. Is git installed and on your PATH?"),
				);
			}
		}
	} else {
		match Command::new("git").arg("pull").current_dir(repo_dir).stdout(Stdio::null()).stderr(Stdio::null()).output()
		{
			Ok(out) => out.status.code().unwrap_or(-1),
			Err(e) => {
				return PullOutcome::Failed(
					anyhow::Error::from(e).context("Could not run 'git pull'. Is git installed and on your PATH?"),
				);
			}
		}
	};
	if exit_code == 0 {
		let head_after = git_head(repo_dir);
		if head_before == head_after { PullOutcome::UpToDate } else { PullOutcome::Updated }
	} else if exit_code == 128 {
		PullOutcome::Fatal
	} else {
		PullOutcome::Failed(anyhow::anyhow!("git pull failed with code {exit_code}."))
	}
}

fn git_clone(url: &str, dest: &Path, verbosity: Verbosity) -> Result<()> {
	if verbosity == Verbosity::Verbose {
		let status = Command::new("git")
			.args(["clone", "--", url])
			.arg(dest)
			.status()
			.context("Could not run 'git clone'. Is git installed and on your PATH?")?;
		if !status.success() {
			bail!("git clone failed with code {}. Check the URL and your credentials.", status.code().unwrap_or(-1));
		}
	} else {
		let out = Command::new("git")
			.args(["clone", "--", url])
			.arg(dest)
			.stdout(Stdio::null())
			.stderr(Stdio::null())
			.output()
			.context("Could not run 'git clone'. Is git installed and on your PATH?")?;
		if !out.status.success() {
			bail!("git clone failed with code {}.", out.status.code().unwrap_or(-1));
		}
	}
	Ok(())
}
