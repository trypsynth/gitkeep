use std::{
	collections::{HashMap, HashSet},
	fmt::Write as _,
	fs,
};

use anyhow::Result;
use futures_util::{StreamExt as _, stream};

use super::{SyncContext, SyncState, Verbosity, repo::Plan};
use crate::{
	config::{Config, TrackedAccount},
	forge::{AccountRepos, Forge, RemoteRepo, archive_path, split_host},
	utils::plural,
};

/// A tracked account or pinned repo whose listing a sync needs from its forge.
pub enum Task<'a> {
	Account(&'a TrackedAccount),
	Pin { full_name: &'a str, stored_id: Option<u64> },
}

pub enum Fetched<'a> {
	Account(&'a TrackedAccount, Result<AccountRepos>),
	Pin { full_name: &'a str, stored_id: Option<u64>, repo: Result<Option<RemoteRepo>> },
}

/// Fetches every task's listing from its forge, up to `limit` requests at once, in task order.
/// Every task's forge must already be in `forges`.
pub async fn fetch_all<'a>(
	forges: &HashMap<Option<String>, Result<Forge>>,
	tasks: Vec<Task<'a>>,
	limit: usize,
) -> Vec<Fetched<'a>> {
	let forge = |host: Option<&str>| match forges.get(&host.map(str::to_string)) {
		Some(Ok(forge)) => forge,
		_ => unreachable!("tasks are only made for hosts with a forge"),
	};
	stream::iter(tasks)
		.map(|task| async move {
			match task {
				Task::Account(user) => {
					Fetched::Account(user, forge(user.host.as_deref()).account(&user.name, user.id).await)
				}
				Task::Pin { full_name, stored_id } => {
					let (host, path) = split_host(full_name).map_or((None, full_name), |(h, p)| (Some(h), p));
					Fetched::Pin { full_name, stored_id, repo: forge(host).repo(path, stored_id).await }
				}
			}
		})
		.buffered(limit)
		.collect()
		.await
}

/// Plans the sync of every repo of one tracked account. Follows account renames (by stable id, where
/// the forge has one), moving the local archive and updating the config. Returns `true` if the
/// config changed.
pub fn sync_one(
	user: &TrackedAccount,
	fetched: Result<AccountRepos>,
	ctx: SyncContext<'_>,
	config: &mut Config,
	sync_state: &mut SyncState<'_>,
	plan: &mut Plan,
) -> bool {
	let display = user.display_name();
	if ctx.verbosity == Verbosity::Verbose {
		println!("Checking {display}...");
	}
	let account = match fetched {
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
	report_gone(&current, &account.repos, ctx, sync_state);
	plan.add(account.repos, include_forks, use_submodules, ctx, config, sync_state);
	config_changed
}

/// Reports the repos of `account` that were synced before but that its forge no longer lists, such
/// as ones deleted or made private. Their local copies stay; they're dropped from the sync state so
/// each is only reported once.
fn report_gone(account: &TrackedAccount, listed: &[RemoteRepo], ctx: SyncContext<'_>, sync_state: &mut SyncState<'_>) {
	let listed: HashSet<&str> = listed.iter().map(|r| r.full_name.as_str()).collect();
	let gone: Vec<String> =
		sync_state.state.repos.keys().filter(|k| account.covers(k) && !listed.contains(k.as_str())).cloned().collect();
	for key in gone {
		sync_state.state.repos.remove(&key);
		if archive_path(ctx.archive_dir, &key).exists() {
			println!("  {key} is gone upstream. The local copy is kept.");
			sync_state.totals.gone += 1;
		}
	}
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

/// Plans the sync of one individually pinned repo. Follows renames (by stable id, where the forge
/// has one), moving the local copy and updating the pin. Returns `true` if the config changed.
pub fn sync_one_pinned(
	full_name: &str,
	stored_id: Option<u64>,
	fetched: Result<Option<RemoteRepo>>,
	ctx: SyncContext<'_>,
	config: &mut Config,
	sync_state: &mut SyncState<'_>,
	plan: &mut Plan,
) -> bool {
	if ctx.verbosity == Verbosity::Verbose {
		println!("Checking {full_name}...");
	}
	let use_submodules =
		resolve_submodules(ctx.opts.force_submodules, config.pinned_submodules(full_name), config.submodules);
	let repo = match fetched {
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
	if repo.id.is_some()
		&& stored_id != repo.id
		&& let Some(pin) = config.pinned.iter_mut().find(|p| p.full_name == repo.full_name)
	{
		pin.id = repo.id;
		config_changed = true;
	}
	plan.add(vec![repo], true, use_submodules, ctx, config, sync_state);
	config_changed
}

/// Resolves whether submodules should be cloned/updated, in priority order: an explicit
/// one-off `--submodules` flag, then a per-account/per-pin override, then the global default.
fn resolve_submodules(force: bool, override_: Option<bool>, global_default: bool) -> bool {
	force || override_.unwrap_or(global_default)
}

#[cfg(test)]
mod tests {
	use std::{env, process};

	use super::{super::SyncOptions, *};
	use crate::{config::State, sync::summary::Totals};

	#[test]
	fn gone_repos_are_reported_once_and_only_when_a_local_copy_exists() {
		let root = env::temp_dir().join(format!("gitkeep-gone-test-{}", process::id()));
		fs::create_dir_all(root.join("alice/old")).unwrap();
		let mut state = State::default();
		for key in ["alice/old", "alice/kept", "alice/deleted-locally", "bob/other"] {
			state.mark_synced(key, None, None);
		}
		let mut totals = Totals::default();
		let mut sync_state = SyncState { state: &mut state, totals: &mut totals };
		let ctx =
			SyncContext { archive_dir: &root, opts: SyncOptions::default(), verbosity: Verbosity::Quiet, jobs: 1 };
		let listed = [RemoteRepo {
			full_name: "alice/kept".into(),
			rel_dir: "alice/kept".into(),
			id: None,
			pushed_at: None,
			fork: false,
			clone_url: None,
			ssh_url: None,
		}];
		report_gone(&TrackedAccount::with_options("alice", false, false), &listed, ctx, &mut sync_state);
		fs::remove_dir_all(&root).unwrap();
		assert_eq!(totals.gone, 1);
		let left: Vec<&String> = state.repos.keys().collect();
		assert_eq!(left, ["alice/kept", "bob/other"]);
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
}
