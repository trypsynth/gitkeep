use std::{
	collections::{HashMap, HashSet},
	fs,
	num::NonZero,
	path::Path,
	thread,
};

use anyhow::{Context, Result, bail};

mod fetch;
mod git;
mod repo;
mod summary;

use self::{
	fetch::{Fetched, Task, fetch_all, sync_one, sync_one_pinned},
	repo::Plan,
	summary::{Totals, build_normal_detail, build_summary},
};
use crate::{
	config::{Config, State, TrackedAccount},
	forge::{Forge, split_host},
	track::{self, AddedItem},
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
	/// How many repos to clone or pull at once, overriding the config.
	pub jobs: Option<usize>,
}

/// How many repos to clone or pull at once when neither `--jobs` nor the config says: one per CPU
/// thread, kept between 4 and 16. Clones mostly wait on the network rather than the CPU, so small
/// machines still benefit from a few at once, and big ones shouldn't flood a forge with connections.
fn default_jobs() -> usize {
	thread::available_parallelism().map_or(8, NonZero::get).clamp(4, 16)
}

/// Read-only settings shared by every repo synced during a single run.
#[derive(Clone, Copy)]
struct SyncContext<'a> {
	archive_dir: &'a Path,
	opts: SyncOptions,
	verbosity: Verbosity,
	/// How many git commands to run at once.
	jobs: usize,
}

enum Item {
	Account(TrackedAccount),
	Pin(String),
}

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
	let untracked: Vec<String> = targets.iter().filter(|t| classify(&config, t).is_none()).cloned().collect();
	if !untracked.is_empty() {
		track::add(&untracked, false, false, false, None).await?;
	}
	let mut config = Config::load().context("Could not load config")?;
	let items: Vec<Item> = targets.iter().filter_map(|target| classify(&config, target)).collect();
	if items.is_empty() {
		println!("Nothing to sync.");
		return Ok(());
	}
	sync_all(&mut config, &items, opts, verbosity).await
}

async fn run_all(opts: SyncOptions, verbosity: Verbosity) -> Result<()> {
	let mut config = Config::load().context("Could not load config")?;
	if config.track.is_empty() && config.pinned.is_empty() {
		bail!(
			"Nothing to sync. Use 'gitkeep add <username>' to start building your library, \
             or run 'gitkeep login' to authenticate and auto-add your account."
		);
	}
	let to_sync: Vec<&TrackedAccount> = config.track.iter().filter(|u| !u.frozen).collect();
	if to_sync.is_empty() && config.pinned.is_empty() {
		println!("All tracked accounts are frozen. Use 'gitkeep sync <account>' to sync one anyway.");
		return Ok(());
	}
	let pinned_to_sync = config
		.pinned
		.iter()
		.filter(|p| !to_sync.iter().any(|t| t.covers(&p.full_name)))
		.map(|p| Item::Pin(p.full_name.clone()));
	let items: Vec<Item> = to_sync.iter().map(|&u| Item::Account(u.clone())).chain(pinned_to_sync).collect();
	sync_all(&mut config, &items, opts, verbosity).await
}

/// Matches a `sync` argument against what's tracked. A repo inside a tracked account means that
/// account, unless the repo was removed from it. Returns `None` for anything untracked.
fn classify(config: &Config, arg: &str) -> Option<Item> {
	let key = config.target(arg).key();
	if let Some(account) = config.track.iter().find(|u| matches_target(u, &key)) {
		return Some(Item::Account(account.clone()));
	}
	if let Some(pin) = config.pinned.iter().find(|p| p.full_name.eq_ignore_ascii_case(&key)) {
		return Some(Item::Pin(pin.full_name.clone()));
	}
	config.track.iter().find(|u| u.covers(&key)).filter(|_| !config.is_excluded(&key)).cloned().map(Item::Account)
}

/// Syncs what `gitkeep add` just added, as one run with one summary. `add` has already saved any
/// `--forks` or submodules choice to the config, so no per-run options are needed.
pub async fn run_added(added: &[AddedItem]) -> Result<()> {
	let mut config = Config::load().context("Could not load config")?;
	let tracked = |name: &str| config.track.iter().find(|u| matches_target(u, name)).cloned();
	let items: Vec<Item> = added
		.iter()
		.filter_map(|item| match item {
			AddedItem::Account(name) | AddedItem::Restored(name) => tracked(name).map(Item::Account),
			AddedItem::Pin(full_name) => Some(Item::Pin(full_name.clone())),
		})
		.collect();
	if items.is_empty() {
		return Ok(());
	}
	sync_all(&mut config, &items, SyncOptions::default(), Verbosity::Normal).await
}

/// Syncs `items` in order, in one run with one summary.
async fn sync_all(config: &mut Config, items: &[Item], opts: SyncOptions, verbosity: Verbosity) -> Result<()> {
	let archive_dir = config.archive_dir()?;
	fs::create_dir_all(&archive_dir)
		.with_context(|| format!("Could not create archive directory: {}", archive_dir.display()))?;
	let mut state = State::load()?;
	let mut config_changed = migrate_legacy_skipped(config, &mut state);
	let mut totals = Totals::default();
	let mut sync_state = SyncState { state: &mut state, totals: &mut totals };
	let mut forges: HashMap<Option<String>, Result<Forge>> = HashMap::new();
	let mut seen = HashSet::new();
	let parallel = opts.jobs.or(config.jobs).unwrap_or_else(default_jobs).max(1);
	// Verbose mode streams git's own output, which would interleave unreadably in parallel.
	let jobs = if verbosity == Verbosity::Verbose { 1 } else { parallel };
	let mut tasks = Vec::new();
	let ctx = SyncContext { archive_dir: &archive_dir, opts, verbosity, jobs };
	for item in items {
		let (host, display) = match item {
			Item::Account(user) => (user.host.clone(), user.display_name()),
			Item::Pin(full_name) => (split_host(full_name).map(|(h, _)| h.to_string()), full_name.clone()),
		};
		if !seen.insert(display.clone()) {
			continue;
		}
		match forges.entry(host.clone()).or_insert_with(|| config.forge(host.as_deref())) {
			Ok(_) => tasks.push(match item {
				Item::Account(user) => Task::Account(user),
				Item::Pin(full_name) => Task::Pin { full_name, stored_id: config.pinned_id(full_name) },
			}),
			Err(e) => {
				eprintln!("  Could not sync {display}: {e:#}.");
				sync_state.totals.failed += 1;
			}
		}
	}
	let mut plan = Plan::default();
	for fetched in fetch_all(&forges, tasks, parallel).await {
		config_changed |= match fetched {
			Fetched::Account(user, repos) => sync_one(user, repos, ctx, config, &mut sync_state, &mut plan),
			Fetched::Pin { full_name, stored_id, repo } => {
				sync_one_pinned(full_name, stored_id, repo, ctx, config, &mut sync_state, &mut plan)
			}
		};
	}
	plan.run(ctx, &mut sync_state);
	state.save().context("Could not save sync state")?;
	if config_changed {
		config.save().context("Could not save config")?;
	}
	if verbosity == Verbosity::Normal
		&& let Some(detail) = build_normal_detail(&totals)
	{
		println!("{detail}");
		println!();
	}
	println!("{}", build_summary(&totals));
	// Exiting non-zero lets a scheduled sync tell whoever runs it that something went wrong.
	if totals.failed > 0 {
		bail!("{} could not be synced", plural(totals.failed, "repo", "repos"));
	}
	Ok(())
}

/// Moves repos removed by versions that kept them in the sync state into the config. Returns `true`
/// if any moved, so the config gets saved along with the state they were taken from.
fn migrate_legacy_skipped(config: &mut Config, state: &mut State) -> bool {
	let legacy = state.drain_legacy_skipped();
	let moved = !legacy.is_empty();
	config.excluded.extend(legacy);
	moved
}

/// True when a `sync <target>` argument names this tracked account: a plain name matches
/// a GitHub account, a `host/path` name matches an account on that host.
fn matches_target(user: &TrackedAccount, target: &str) -> bool {
	user.display_name().eq_ignore_ascii_case(target)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn legacy_skipped_repos_move_into_config_and_mark_it_changed() {
		let mut config = Config::default();
		let mut state = State::default();
		assert!(!migrate_legacy_skipped(&mut config, &mut state));
		state.skipped.insert("alice/big".to_string());
		assert!(migrate_legacy_skipped(&mut config, &mut state));
		assert!(config.is_excluded("alice/big"));
		assert!(state.skipped.is_empty());
	}

	fn named(config: &Config, arg: &str) -> String {
		match classify(config, arg) {
			Some(Item::Account(a)) => format!("account {}", a.display_name()),
			Some(Item::Pin(p)) => format!("pin {p}"),
			None => "untracked".to_string(),
		}
	}

	#[test]
	fn classify_matches_frozen_accounts_too() {
		let mut config = Config::default();
		config.add_account(None, "Alice", false, true, false, None);
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
		config.add_account(None, "alice", false, false, false, None);
		assert_eq!(named(&config, "alice/repo"), "account alice");
	}

	#[test]
	fn classify_treats_removed_repo_as_untracked() {
		let mut config = Config::default();
		config.add_account(None, "alice", false, false, false, None);
		config.exclude_repo("alice/big");
		assert_eq!(named(&config, "alice/big"), "untracked");
	}

	#[test]
	fn classify_distinguishes_hosts() {
		let mut config = Config::default();
		config.add_account(Some("gitlab.example.com"), "alice", false, false, false, None);
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
