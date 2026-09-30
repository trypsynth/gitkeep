use std::{fs, path::Path};

use chrono::{DateTime, Utc};

use super::{
	SyncContext, SyncState, Verbosity,
	git::{PullOutcome, git_clone, git_pull, update_submodules},
};
use crate::{config::Config, forge::RemoteRepo};

/// Identifying details for a single repo being cloned or pulled.
struct RepoInfo<'a> {
	full_name: &'a str,
	id: u64,
	pushed_at: Option<DateTime<Utc>>,
}

pub fn sync_repo_list(
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
	fn pull_when_neither_pushed_at() {
		assert!(!should_skip_pull(None, None));
	}
}
