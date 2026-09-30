use std::{
	collections::{HashMap, hash_map::Entry},
	fmt::Write as _,
	fs,
	path::{Path, PathBuf},
};

use anyhow::Result;

use crate::{
	config::{Config, HostConfig, PinnedRepo, TrackedUser},
	forge::{self, Forge, Resolved, Target, archive_path},
	utils::{confirm, plural},
};

/// What `add` recorded, by key, so the caller can sync it right away.
#[derive(Default)]
pub struct Added {
	/// Accounts that are now tracked.
	pub accounts: Vec<String>,
	/// Repos that are now individually pinned.
	pub pinned: Vec<String>,
	/// Tracked accounts that had a previously removed repo added back.
	pub restored_owners: Vec<String>,
}

/// Adds accounts and repos from any forge. Each target is a GitHub name (`owner` or
/// `owner/repo`), a host-qualified name (`host/path`), or a URL. The first time a host is seen, its
/// forge kind is detected and recorded.
pub async fn add(targets: &[String], forks: bool, frozen: bool, submodules: Option<bool>) -> Result<Added> {
	let mut config = Config::load()?;
	let mut changed = false;
	let mut forges: HashMap<Option<String>, Forge> = HashMap::new();
	let mut resolved = Vec::with_capacity(targets.len());
	for arg in targets {
		let target = Target::parse(arg);
		if let Some(host) = &target.host
			&& !config.hosts.contains_key(host)
		{
			let kind = forge::detect(host).await?;
			println!("Detected {host} as {}.", kind.name());
			config.hosts.insert(host.clone(), HostConfig { kind, token: None });
			changed = true;
		}
		let forge = match forges.entry(target.host.clone()) {
			Entry::Occupied(e) => e.into_mut(),
			Entry::Vacant(e) => e.insert(config.forge(target.host.as_deref())?),
		};
		resolved.push((target.host, forge.resolve(&target.path).await?));
	}
	let mut added = Added::default();
	// Record accounts before repos, so `gitkeep add rust-lang rust-lang/mdBook` tracks the org and
	// skips the now-redundant pin.
	for (host, item) in &resolved {
		if let Resolved::Account(name) = item {
			changed |= config.add_user_on(host.as_deref(), name, forks, frozen, submodules);
			let tracked = TrackedUser { host: host.clone(), ..TrackedUser::with_options(name, forks, frozen) };
			for pin in config.remove_pins_covered(&tracked) {
				println!("{pin} removed (now covered by {}).", tracked.display_name());
				changed = true;
			}
			added.accounts.push(tracked.display_name());
		}
	}
	for (_, item) in resolved {
		let Resolved::Repo(repo) = item else { continue };
		let key = repo.full_name;
		if let Some(owner) = config.track.iter().find(|u| u.covers(&key)).map(TrackedUser::display_name) {
			if let Some(restored) = config.include_repo(&key) {
				println!("Now tracking {restored} again.");
				changed = true;
				if !added.restored_owners.contains(&owner) {
					added.restored_owners.push(owner);
				}
			} else {
				println!("{owner} is already fully tracked; {key} will be synced automatically.");
			}
		} else if config.is_pinned(&key) {
			println!("Already tracking {key}.");
		} else {
			// A leftover exclusion for an account that's no longer tracked means nothing; drop it.
			config.include_repo(&key);
			config.pin_repo_with_options(&key, Some(repo.id), submodules);
			println!("Now tracking {key}.");
			added.pinned.push(key);
			changed = true;
		}
	}
	if changed {
		config.save()?;
	}
	Ok(added)
}

/// Stops tracking accounts or repos on any forge, offering to delete their local copies.
pub async fn remove(targets: &[String], delete_dir: bool, yes: bool) -> Result<()> {
	let mut config = Config::load()?;
	let archive_root = config.archive_dir()?;
	let mut changed = false;
	for arg in targets {
		changed |= remove_one(&mut config, &archive_root, &Target::parse(arg), delete_dir, yes).await?;
	}
	if changed {
		config.save()?;
	}
	Ok(())
}

/// Removes one target: a tracked account, a pinned repo, or a single repo under a tracked account
/// (which is excluded from syncs). Returns `true` if the config changed.
async fn remove_one(
	config: &mut Config,
	archive_root: &Path,
	target: &Target,
	delete_dir: bool,
	yes: bool,
) -> Result<bool> {
	let key = target.key();
	let host = target.host.as_deref();
	let account = config.track.iter().find(|u| u.host.as_deref() == host && u.name.eq_ignore_ascii_case(&target.path));
	if let Some(account) = account.cloned() {
		config.remove_user(host, &account.name);
		config.remove_exclusions_covered(&account);
		let name = account.display_name();
		let dir = archive_path(archive_root, &name);
		if dir.exists() && (delete_dir || yes || confirm(&format!("Delete local archive for {name}?"), false)?) {
			println!("Deleting {}...", dir.display());
			fs::remove_dir_all(&dir)?;
		}
		return Ok(true);
	}
	if let Some(pin) = config.unpin_repo(&key) {
		println!("No longer tracking {pin}.");
		let dir = archive_path(archive_root, &pin);
		if delete_dir && dir.exists() {
			println!("Deleting {}...", dir.display());
			fs::remove_dir_all(&dir)?;
		}
		return Ok(true);
	}
	if target.path.contains('/')
		&& let Some(owner) = config.track.iter().find(|u| u.covers(&key)).cloned()
	{
		return exclude_repo(config, archive_root, &owner, target, delete_dir || yes).await;
	}
	remove_untracked(config, archive_root, &key, target, delete_dir, yes)
}

/// Splits a key into the archive directory that holds it and its last path segment.
fn parent_and_name<'a>(archive_root: &Path, key: &'a str) -> (PathBuf, &'a str) {
	key.rsplit_once('/')
		.map_or_else(|| (archive_root.to_path_buf(), key), |(parent, name)| (archive_path(archive_root, parent), name))
}

/// Handles a target that isn't tracked itself: it may still have individually pinned repos under
/// it, or a leftover local archive from before it was removed.
fn remove_untracked(
	config: &mut Config,
	archive_root: &Path,
	key: &str,
	target: &Target,
	delete_dir: bool,
	yes: bool,
) -> Result<bool> {
	let as_account = TrackedUser { host: target.host.clone(), ..TrackedUser::with_options(&target.path, false, false) };
	let mut matching: Vec<String> =
		config.pinned.iter().filter(|p| as_account.covers(&p.full_name)).map(|p| p.full_name.clone()).collect();
	matching.sort();
	let (parent, name) = parent_and_name(archive_root, key);
	if matching.is_empty() {
		match find_dir_ignoring_case(&parent, name)? {
			Some(dir) => {
				println!("'{key}' is not tracked, but a local archive exists at {}.", dir.display());
				if delete_dir || yes || confirm("Delete it?", false)? {
					println!("Deleting {}...", dir.display());
					fs::remove_dir_all(&dir)?;
				}
			}
			None => println!("Not tracking '{key}'."),
		}
		return Ok(false);
	}
	println!(
		"'{key}' is not tracked, but you have {} individually tracked under it:",
		plural(matching.len(), "repo", "repos")
	);
	for repo in &matching {
		println!("  {repo}");
	}
	if !yes && !confirm("Remove these repos too?", false)? {
		return Ok(false);
	}
	for repo in config.remove_pins_covered(&as_account) {
		println!("No longer tracking {repo}.");
		let repo_dir = archive_path(archive_root, &repo);
		if repo_dir.exists() && (delete_dir || yes || confirm(&format!("Delete local archive for {repo}?"), false)?) {
			println!("Deleting {}...", repo_dir.display());
			fs::remove_dir_all(&repo_dir)?;
		}
	}
	// Clean up the owner's directory if that left it empty.
	if let Some(dir) = find_dir_ignoring_case(&parent, name)? {
		let _ = fs::remove_dir(&dir);
	}
	Ok(true)
}

/// Removes a single repo under the fully tracked account `owner` by excluding it from syncs, then
/// offers to delete its local copy. Returns `true` if the config changed.
async fn exclude_repo(
	config: &mut Config,
	archive_root: &Path,
	owner: &TrackedUser,
	target: &Target,
	delete: bool,
) -> Result<bool> {
	let key = target.key();
	let Some((parent, name)) = key.rsplit_once('/') else { return Ok(false) };
	// Use the account's canonical casing for the owner part when the repo sits directly under it.
	let owner_key = owner.display_name();
	let parent = if parent.eq_ignore_ascii_case(&owner_key) { owner_key } else { parent.to_string() };
	let local_dir = find_dir_ignoring_case(&archive_path(archive_root, &parent), name)?;
	// Prefer names we already know over a forge lookup, so repos deleted upstream can still be removed.
	let full_name = if let Some(dir) = &local_dir {
		format!("{parent}/{}", dir.file_name().map_or_else(|| name.into(), |n| n.to_string_lossy()))
	} else if let Some(existing) = config.excluded.iter().find(|r| r.eq_ignore_ascii_case(&key)) {
		existing.clone()
	} else {
		let forge_name = target.host.as_deref().unwrap_or("GitHub");
		match config.forge(target.host.as_deref())?.repo(&target.path, None).await {
			Ok(Some(repo)) => repo.full_name,
			Ok(None) => {
				println!("'{key}' does not exist on {forge_name}.");
				return Ok(false);
			}
			Err(e) => {
				println!("Could not remove '{key}': {e:#}.");
				return Ok(false);
			}
		}
	};
	apply_exclusion(config, &full_name, local_dir, delete)
}

/// Records `full_name` as excluded and offers to delete its local copy. Returns `true` if the
/// config changed.
fn apply_exclusion(config: &mut Config, full_name: &str, local_dir: Option<PathBuf>, delete: bool) -> Result<bool> {
	let changed = config.exclude_repo(full_name);
	if changed {
		println!("{full_name} will no longer be synced.");
	} else {
		println!("Already removed {full_name}.");
	}
	if let Some(dir) = local_dir
		&& (delete || confirm(&format!("Delete local archive for {full_name}?"), false)?)
	{
		println!("Deleting {}...", dir.display());
		fs::remove_dir_all(&dir)?;
	}
	Ok(changed)
}

/// Looks for a directory directly under `parent` matching `name` (case-insensitively, since GitHub
/// names aren't case-sensitive but directory lookups on most filesystems are). Used to find the
/// local copy of an account or repo being removed.
fn find_dir_ignoring_case(parent: &Path, name: &str) -> Result<Option<PathBuf>> {
	if !parent.is_dir() {
		return Ok(None);
	}
	// Scan instead of checking `parent.join(name)` directly: on case-insensitive filesystems that
	// would succeed with the caller's casing rather than the directory's real name.
	let mut fallback = None;
	for entry in fs::read_dir(parent)? {
		let entry = entry?;
		if !entry.file_type()?.is_dir() {
			continue;
		}
		let file_name = entry.file_name();
		let file_name = file_name.to_string_lossy();
		if file_name == name {
			return Ok(Some(entry.path()));
		}
		if fallback.is_none() && file_name.eq_ignore_ascii_case(name) {
			fallback = Some(entry.path());
		}
	}
	Ok(fallback)
}

fn format_list(config: &Config) -> String {
	let mut out = String::new();
	if config.track.is_empty() && config.pinned.is_empty() {
		return "No users tracked. Use 'gitkeep add <username>' to start.\n".to_string();
	}
	if !config.track.is_empty() {
		let _ = writeln!(out, "Tracked users and orgs ({} total):", config.track.len());
		for user in &config.track {
			let mut tags = Vec::new();
			if user.forks {
				tags.push("forks");
			}
			if user.frozen {
				tags.push("frozen");
			}
			match user.submodules {
				Some(true) => tags.push("submodules"),
				Some(false) => tags.push("no-submodules"),
				None => {}
			}
			let suffix = if tags.is_empty() { String::new() } else { format!(" [{}]", tags.join(", ")) };
			let _ = writeln!(out, "  {}{}", user.display_name(), suffix);
		}
	}
	if !config.pinned.is_empty() {
		let mut sorted: Vec<&PinnedRepo> = config.pinned.iter().collect();
		sorted.sort_by(|a, b| a.full_name.cmp(&b.full_name));
		if !out.is_empty() {
			out.push('\n');
		}
		let _ = writeln!(out, "Repos ({} total):", sorted.len());
		for repo in sorted {
			let suffix = match repo.submodules {
				Some(true) => " [submodules]",
				Some(false) => " [no-submodules]",
				None => "",
			};
			let _ = writeln!(out, "  {}{}", repo.full_name, suffix);
		}
	}
	if !config.excluded.is_empty() {
		let mut sorted: Vec<&String> = config.excluded.iter().collect();
		sorted.sort_by_key(|r| r.to_lowercase());
		let _ = writeln!(
			out,
			"
Removed repos ({} total):",
			sorted.len()
		);
		for repo in sorted {
			let _ = writeln!(out, "  {repo}");
		}
	}
	out
}

pub fn list() -> Result<()> {
	let config = Config::load()?;
	print!("{}", format_list(&config));
	Ok(())
}

#[cfg(test)]
mod tests {
	use std::{
		env, process,
		sync::atomic::{AtomicU32, Ordering},
	};

	use super::*;

	/// Creates a fresh, empty scratch directory under the system temp dir for a single test.
	fn temp_dir() -> PathBuf {
		static COUNTER: AtomicU32 = AtomicU32::new(0);
		let n = COUNTER.fetch_add(1, Ordering::Relaxed);
		let dir = env::temp_dir().join(format!("gitkeep-track-test-{}-{n}", process::id()));
		fs::create_dir_all(&dir).unwrap();
		dir
	}

	fn gitlab_user(name: &str) -> TrackedUser {
		TrackedUser { host: Some("gitlab.example.com".to_string()), ..TrackedUser::with_options(name, false, false) }
	}

	async fn remove(config: &mut Config, root: &Path, target: &str, delete_dir: bool) -> bool {
		remove_one(config, root, &Target::parse(target), delete_dir, true).await.unwrap()
	}

	#[tokio::test]
	async fn remove_excludes_repo_using_local_casing_and_deletes() {
		let root = temp_dir();
		fs::create_dir_all(root.join("Alice").join("BigRepo")).unwrap();
		let mut config = Config::default();
		config.add_user("Alice", false, false, None);
		assert!(remove(&mut config, &root, "alice/bigrepo", true).await);
		assert!(config.excluded.contains("Alice/BigRepo"));
		assert_eq!(config.track.len(), 1, "the account itself stays tracked");
		assert!(!root.join("Alice").join("BigRepo").exists());
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn remove_already_excluded_repo_is_unchanged() {
		let root = temp_dir();
		let mut config = Config::default();
		config.add_user("alice", false, false, None);
		config.exclude_repo("alice/big");
		assert!(!remove(&mut config, &root, "alice/big", true).await);
		assert_eq!(config.excluded.len(), 1);
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn remove_excludes_gitlab_project_under_tracked_group() {
		let root = temp_dir();
		let project_dir = root.join("gitlab.example.com").join("grp").join("sub").join("big");
		fs::create_dir_all(&project_dir).unwrap();
		let mut config = Config::default();
		config.track.push(gitlab_user("grp"));
		assert!(remove(&mut config, &root, "https://gitlab.example.com/grp/sub/big", true).await);
		assert!(config.is_excluded("gitlab.example.com/grp/sub/big"));
		assert_eq!(config.track.len(), 1, "the group itself stays tracked");
		assert!(!project_dir.exists());
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn remove_gitlab_group_clears_its_exclusions_only() {
		let root = temp_dir();
		let mut config = Config::default();
		config.track.push(gitlab_user("grp"));
		config.exclude_repo("gitlab.example.com/grp/big");
		config.exclude_repo("gitlab.example.com/other/big");
		config.exclude_repo("alice/big");
		assert!(remove(&mut config, &root, "gitlab.example.com/grp", false).await);
		assert!(config.track.is_empty());
		assert!(!config.is_excluded("gitlab.example.com/grp/big"));
		assert!(config.is_excluded("gitlab.example.com/other/big"));
		assert!(config.is_excluded("alice/big"));
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn remove_github_account_leaves_same_name_on_other_hosts() {
		let root = temp_dir();
		let mut config = Config::default();
		config.add_user("alice", false, false, None);
		config.track.push(gitlab_user("alice"));
		assert!(remove(&mut config, &root, "alice", false).await);
		assert_eq!(config.track.len(), 1);
		assert_eq!(config.track[0].host.as_deref(), Some("gitlab.example.com"));
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn remove_ignores_repo_of_untracked_owner() {
		let root = temp_dir();
		let mut config = Config::default();
		assert!(!remove(&mut config, &root, "bob/repo", true).await);
		assert!(config.excluded.is_empty());
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn remove_unpins_pinned_repo_instead_of_excluding() {
		let root = temp_dir();
		let mut config = Config::default();
		config.pin_repo_with_options("bob/repo", None, None);
		assert!(remove(&mut config, &root, "Bob/Repo", false).await);
		assert!(!config.is_pinned("bob/repo"));
		assert!(config.excluded.is_empty());
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn remove_untracked_owner_drops_its_pins() {
		let root = temp_dir();
		let mut config = Config::default();
		config.pin_repo_with_options("gitlab.example.com/grp/a", None, None);
		config.pin_repo_with_options("gitlab.example.com/other/b", None, None);
		assert!(remove(&mut config, &root, "gitlab.example.com/grp", false).await);
		assert!(!config.is_pinned("gitlab.example.com/grp/a"));
		assert!(config.is_pinned("gitlab.example.com/other/b"));
		fs::remove_dir_all(&root).unwrap();
	}

	#[test]
	fn find_dir_ignoring_case_matches_exact_case() {
		let root = temp_dir();
		fs::create_dir(root.join("alice")).unwrap();
		let found = find_dir_ignoring_case(&root, "alice").unwrap();
		assert_eq!(found, Some(root.join("alice")));
		fs::remove_dir_all(&root).unwrap();
	}

	#[test]
	fn find_dir_ignoring_case_matches_case_insensitively() {
		let root = temp_dir();
		fs::create_dir(root.join("Alice")).unwrap();
		let found = find_dir_ignoring_case(&root, "alice").unwrap();
		assert_eq!(found, Some(root.join("Alice")));
		fs::remove_dir_all(&root).unwrap();
	}

	#[test]
	fn find_dir_ignoring_case_none_when_missing() {
		let root = temp_dir();
		let found = find_dir_ignoring_case(&root, "alice").unwrap();
		assert_eq!(found, None);
		fs::remove_dir_all(&root).unwrap();
	}

	#[test]
	fn find_dir_ignoring_case_none_when_archive_root_missing() {
		let root = temp_dir().join("does-not-exist");
		let found = find_dir_ignoring_case(&root, "alice").unwrap();
		assert_eq!(found, None);
	}

	#[test]
	fn list_empty_state_shows_hint() {
		let config = Config::default();
		let out = format_list(&config);
		assert!(out.contains("gitkeep add"), "got: {out}");
	}

	#[test]
	fn list_pinned_only_does_not_show_hint() {
		let mut config = Config::default();
		config.pin_repo_with_options("alice/repo", None, None);
		let out = format_list(&config);
		assert!(!out.contains("gitkeep add"), "got: {out}");
	}

	#[test]
	fn list_shows_tracked_users() {
		let mut config = Config::default();
		config.add_user("alice", false, false, None);
		let out = format_list(&config);
		assert!(out.contains("alice"), "got: {out}");
	}

	#[test]
	fn list_shows_forks_tag() {
		let mut config = Config::default();
		config.add_user("alice", true, false, None);
		let out = format_list(&config);
		assert!(out.contains("forks"), "got: {out}");
	}

	#[test]
	fn list_shows_frozen_tag() {
		let mut config = Config::default();
		config.add_user("alice", false, true, None);
		let out = format_list(&config);
		assert!(out.contains("frozen"), "got: {out}");
	}

	#[test]
	fn list_omits_removed_section_when_none() {
		let mut config = Config::default();
		config.add_user("alice", false, false, None);
		let out = format_list(&config);
		assert!(!out.to_lowercase().contains("removed"), "got: {out}");
	}

	#[test]
	fn list_shows_removed_section_when_present() {
		let mut config = Config::default();
		config.add_user("alice", false, false, None);
		config.exclude_repo("alice/noisy");
		let out = format_list(&config);
		assert!(out.contains("alice/noisy"), "got: {out}");
		assert!(out.to_lowercase().contains("removed"), "got: {out}");
	}

	#[test]
	fn list_removed_repos_are_sorted() {
		let mut config = Config::default();
		config.add_user("alice", false, false, None);
		config.exclude_repo("alice/zzz");
		config.exclude_repo("alice/aaa");
		let out = format_list(&config);
		let aaa_pos = out.find("alice/aaa").unwrap();
		let zzz_pos = out.find("alice/zzz").unwrap();
		assert!(aaa_pos < zzz_pos, "got: {out}");
	}

	#[test]
	fn list_shows_pinned_section() {
		let mut config = Config::default();
		config.pin_repo_with_options("rust-lang/mdBook", None, None);
		let out = format_list(&config);
		assert!(out.contains("rust-lang/mdBook"), "got: {out}");
		assert!(out.contains("Repos ("), "got: {out}");
	}

	#[test]
	fn list_shows_submodules_tag_when_enabled() {
		let mut config = Config::default();
		config.add_user("alice", false, false, Some(true));
		let out = format_list(&config);
		assert!(out.contains("submodules"), "got: {out}");
	}

	#[test]
	fn list_shows_no_submodules_tag_when_explicitly_disabled() {
		let mut config = Config::default();
		config.add_user("alice", false, false, Some(false));
		let out = format_list(&config);
		assert!(out.contains("no-submodules"), "got: {out}");
	}

	#[test]
	fn list_omits_submodules_tag_when_unset() {
		let mut config = Config::default();
		config.add_user("alice", false, false, None);
		let out = format_list(&config);
		assert!(!out.contains("submodules"), "got: {out}");
	}

	#[test]
	fn list_shows_submodules_tag_for_pinned_repo() {
		let mut config = Config::default();
		config.pin_repo_with_options("alice/repo", None, Some(true));
		let out = format_list(&config);
		assert!(out.contains("alice/repo"), "got: {out}");
		assert!(out.contains("submodules"), "got: {out}");
	}

	#[test]
	fn list_pinned_repos_are_sorted() {
		let mut config = Config::default();
		config.pin_repo_with_options("rust-lang/zzz", None, None);
		config.pin_repo_with_options("rust-lang/aaa", None, None);
		let out = format_list(&config);
		let aaa_pos = out.find("rust-lang/aaa").unwrap();
		let zzz_pos = out.find("rust-lang/zzz").unwrap();
		assert!(aaa_pos < zzz_pos, "got: {out}");
	}

	#[test]
	fn list_omits_pinned_section_when_none() {
		let mut config = Config::default();
		config.add_user("alice", false, false, None);
		let out = format_list(&config);
		assert!(!out.contains("Repos ("), "got: {out}");
	}

	#[test]
	fn list_shows_host_qualified_gitlab_entries() {
		let mut config = Config::default();
		config.add_user_on(Some("gitlab.example.com"), "some-group", false, false, None);
		let out = format_list(&config);
		assert!(out.contains("gitlab.example.com/some-group"), "got: {out}");
	}

	#[test]
	fn add_user_removes_pins_for_that_user() {
		let mut config = Config::default();
		config.pin_repo_with_options("alice/foo", None, None);
		config.pin_repo_with_options("alice/bar", None, None);
		config.pin_repo_with_options("bob/baz", None, None);
		config.add_user("alice", false, false, None);
		let pins_removed = config.remove_pins_covered(&TrackedUser::with_options("alice", false, false));
		assert_eq!(pins_removed.len(), 2);
		assert!(config.is_pinned("bob/baz"));
	}
}
