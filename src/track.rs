use std::{
	fmt::Write as _,
	fs,
	path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use octocrab::Octocrab;

use crate::{
	config::{Config, PinnedRepo, TrackedUser},
	gitlab,
	utils::{confirm, plural},
};

/// What `add_gitlab` ended up adding, so the caller can decide what to sync.
pub enum GitLabAddition {
	/// A tracked namespace, as a host-qualified name (`host/path`).
	Namespace(String),
	/// A pinned project, as a host-qualified full name (`host/namespace/project`).
	Project(String),
	/// Nothing new (already tracked or pinned).
	None,
}

/// Resolves a GitLab URL target to a group/user namespace (tracked) or a single project
/// (pinned) and records it in the config.
pub async fn add_gitlab(
	host: &str,
	path: &str,
	forks: bool,
	frozen: bool,
	submodules: Option<bool>,
) -> Result<GitLabAddition> {
	let mut config = Config::load()?;
	let client = gitlab::GitLabClient::new(host, config.gitlab_token(host))?;
	match client.resolve_target(path).await? {
		gitlab::Target::Namespace(full_path) => {
			let display = format!("{host}/{full_path}");
			let mut changed = config.add_user_on(Some(host), &full_path, forks, frozen, submodules);
			let tracked =
				TrackedUser { host: Some(host.to_string()), ..TrackedUser::with_options(&full_path, forks, frozen) };
			// Auto-remove any individually-pinned projects now covered by the namespace.
			for pin in config.remove_pins_covered(&tracked) {
				println!("{pin} removed (now covered by {display}).");
				changed = true;
			}
			if changed {
				config.save()?;
			}
			Ok(GitLabAddition::Namespace(display))
		}
		gitlab::Target::Project(project) => {
			let full_name = format!("{host}/{}", project.path_with_namespace);
			if let Some(owner) = config.track.iter().find(|u| u.covers(&full_name)).map(TrackedUser::display_name) {
				if let Some(restored) = config.include_repo(&full_name) {
					println!("Now tracking {restored} again.");
					config.save()?;
					return Ok(GitLabAddition::Namespace(owner));
				}
				println!("{owner} is already fully tracked; {full_name} will be synced automatically.");
				return Ok(GitLabAddition::None);
			}
			if config.is_pinned(&full_name) {
				println!("Already tracking {full_name}.");
				return Ok(GitLabAddition::None);
			}
			// A leftover exclusion for an account that's no longer tracked means nothing; drop it.
			config.include_repo(&full_name);
			config.pin_repo_with_options(&full_name, Some(project.id), submodules);
			println!("Now tracking {full_name}.");
			config.save()?;
			Ok(GitLabAddition::Project(full_name))
		}
	}
}

pub fn add(users: &[String], forks: bool, frozen: bool, submodules: Option<bool>) -> Result<()> {
	let mut config = Config::load()?;
	let mut changed = false;
	for user in users {
		if config.add_user(user, forks, frozen, submodules) {
			changed = true;
		}
		// Auto-remove any individually-pinned repos from this user — they're now covered.
		for pin in config.remove_pins_for_user(user) {
			println!("{pin} removed (now covered by {user}).");
			changed = true;
		}
	}
	if changed {
		config.save()?;
	}
	Ok(())
}

/// Result of `add_pinned`: repos that should be synced right away.
#[derive(Default)]
pub struct AddedRepos {
	/// Canonical names of newly pinned repos.
	pub pinned: Vec<String>,
	/// Owners of previously removed repos that were added back under a fully tracked account.
	pub restored_owners: Vec<String>,
}

/// Validates and pins individual repos (`user/repo` format). A repo under a fully tracked account
/// that was previously removed is added back instead.
pub async fn add_pinned(repos: &[String], client: &Octocrab, submodules: Option<bool>) -> Result<AddedRepos> {
	let mut config = Config::load()?;
	let mut added = AddedRepos::default();
	let mut changed = false;
	for repo_str in repos {
		let (user, name) = parse_repo_arg(repo_str)?;
		if let Some(tracked) = config.track.iter().find(|u| u.name.eq_ignore_ascii_case(user)).map(|u| u.name.clone()) {
			if let Some(restored) = config.include_repo(repo_str) {
				println!("Now tracking {restored} again.");
				changed = true;
				if !added.restored_owners.contains(&tracked) {
					added.restored_owners.push(tracked);
				}
			} else {
				println!("{tracked} is already fully tracked; {name} will be synced automatically.");
			}
			continue;
		}
		// Case-insensitive duplicate-pin check (before hitting the API).
		if let Some(existing) = config.pinned.iter().find(|p| p.full_name.eq_ignore_ascii_case(repo_str)) {
			println!("Already tracking {}.", existing.full_name);
			continue;
		}
		// Verify the repo exists on GitHub and get canonical casing.
		let (full_name, id) = match client.repos(user, name).get().await {
			Ok(r) => (r.full_name.unwrap_or_else(|| repo_str.clone()), r.id.into_inner()),
			Err(_) => bail!("'{repo_str}' does not exist on GitHub."),
		};
		// Re-check with canonical name in case casing differed.
		if config.is_pinned(&full_name) {
			println!("Already tracking {full_name}.");
			continue;
		}
		// A leftover exclusion for an account that's no longer tracked means nothing; drop it.
		config.include_repo(&full_name);
		config.pin_repo_with_options(&full_name, Some(id), submodules);
		println!("Now tracking {full_name}.");
		added.pinned.push(full_name);
		changed = true;
	}
	if changed {
		config.save()?;
	}
	Ok(added)
}

fn parse_repo_arg(s: &str) -> Result<(&str, &str)> {
	match s.split_once('/') {
		Some((user, name)) if !user.is_empty() && !name.is_empty() && !name.contains('/') => Ok((user, name)),
		_ => bail!("'{s}' is not in user/repo format"),
	}
}

pub async fn remove(users: &[String], delete_dir: bool, yes: bool) -> Result<()> {
	let mut config = Config::load()?;
	let mut changed = false;
	let archive_root = config.archive_dir()?;
	for target in users {
		// Accept both URL form and the host-qualified form shown by `gitkeep list`.
		let target = &gitlab::parse_remote_url(target).map_or_else(|| target.clone(), |(h, p)| format!("{h}/{p}"));
		if let Some((host, path)) = gitlab::split_host(target) {
			if remove_gitlab(&mut config, &archive_root, host, path, target, delete_dir, yes).await? {
				changed = true;
			}
			continue;
		}
		if target.contains('/') {
			if remove_repo(&mut config, &archive_root, target, delete_dir, yes).await? {
				changed = true;
			}
		} else if let Some(canonical) =
			config.track.iter().find(|u| u.name.eq_ignore_ascii_case(target)).map(|u| u.name.clone())
		{
			if config.remove_user(target) {
				changed = true;
				config.remove_exclusions_for_user(&canonical);
				let user_dir = archive_root.join(&canonical);
				if user_dir.exists() {
					let should_delete = if delete_dir || yes {
						true
					} else {
						confirm(&format!("Delete local archive for {target}?"), false)?
					};
					if should_delete {
						println!("Deleting {}...", user_dir.display());
						fs::remove_dir_all(&user_dir)?;
					}
				}
			}
		} else {
			// Not a tracked user — but individually-pinned repos under this user may exist.
			let matching = config.pinned_repos_for_user(target);
			if matching.is_empty() {
				if let Some(dir) = find_dir_ignoring_case(&archive_root, target)? {
					println!("'{target}' is not tracked, but a local archive exists at {}.", dir.display());
					let should_delete = if delete_dir || yes { true } else { confirm("Delete it?", false)? };
					if should_delete {
						println!("Deleting {}...", dir.display());
						fs::remove_dir_all(&dir)?;
					}
				} else {
					println!("Not tracking '{target}'.");
				}
				continue;
			}
			println!(
				"'{target}' is not tracked, but you have {} individually tracked under it:",
				plural(matching.len(), "repo", "repos")
			);
			for repo in &matching {
				println!("  {repo}");
			}
			if !yes && !confirm("Remove these repos too?", false)? {
				continue;
			}
			let mut canonical_user = None;
			for repo in config.remove_pins_for_user(target) {
				println!("No longer tracking {repo}.");
				changed = true;
				let Some((user, name)) = repo.split_once('/') else { continue };
				canonical_user.get_or_insert_with(|| user.to_string());
				let repo_dir = archive_root.join(user).join(name);
				if repo_dir.exists() {
					let should_delete = if delete_dir || yes {
						true
					} else {
						confirm(&format!("Delete local archive for {repo}?"), false)?
					};
					if should_delete {
						println!("Deleting {}...", repo_dir.display());
						fs::remove_dir_all(&repo_dir)?;
					}
				}
			}
			// Clean up the now-possibly-empty top-level user directory.
			if let Some(user) = canonical_user {
				let user_dir = archive_root.join(user);
				if user_dir.is_dir() {
					let _ = fs::remove_dir(&user_dir);
				}
			}
		}
	}
	if changed {
		config.save()?;
	}
	Ok(())
}

/// Removes a host-qualified GitLab target: a tracked namespace if one matches, otherwise
/// a pinned project. Returns whether the config changed.
async fn remove_gitlab(
	config: &mut Config,
	archive_root: &Path,
	host: &str,
	path: &str,
	target: &str,
	delete_dir: bool,
	yes: bool,
) -> Result<bool> {
	let tracked =
		config.track.iter().find(|u| u.host.as_deref() == Some(host) && u.name.eq_ignore_ascii_case(path)).cloned();
	if let Some(tracked) = tracked {
		let canonical = &tracked.name;
		config.track.retain(|u| !(u.host.as_deref() == Some(host) && u.name.eq_ignore_ascii_case(path)));
		config.remove_exclusions_covered(&tracked);
		println!("Stopped tracking {host}/{canonical}.");
		let mut dir = archive_root.join(host);
		dir.extend(canonical.split('/'));
		if dir.exists() {
			let should_delete =
				if delete_dir || yes { true } else { confirm(&format!("Delete local archive for {target}?"), false)? };
			if should_delete {
				println!("Deleting {}...", dir.display());
				fs::remove_dir_all(&dir)?;
			}
		}
		return Ok(true);
	}
	if config.unpin_repo(target) {
		println!("No longer tracking {target}.");
		if delete_dir {
			let mut dir = archive_root.to_path_buf();
			dir.extend(target.split('/'));
			if dir.exists() {
				println!("Deleting {}...", dir.display());
				fs::remove_dir_all(&dir)?;
			}
		}
		return Ok(true);
	}
	if config.track.iter().any(|u| u.covers(target)) {
		return exclude_gitlab_project(config, archive_root, host, path, target, delete_dir || yes).await;
	}
	println!("Not tracking '{target}'.");
	Ok(false)
}

/// Handles `gitkeep remove user/repo`: unpins an individually tracked repo, or excludes a repo
/// under a fully tracked account. Returns `true` if the config changed.
async fn remove_repo(
	config: &mut Config,
	archive_root: &Path,
	target: &str,
	delete_dir: bool,
	yes: bool,
) -> Result<bool> {
	if config.unpin_repo(target) {
		println!("No longer tracking {target}.");
		if delete_dir {
			let (user, name) = parse_repo_arg(target)?;
			let repo_dir = archive_root.join(user).join(name);
			if repo_dir.exists() {
				println!("Deleting {}...", repo_dir.display());
				fs::remove_dir_all(&repo_dir)?;
			}
		}
		return Ok(true);
	}
	let (user, _) = parse_repo_arg(target)?;
	let Some(owner) = config.track.iter().find(|u| u.name.eq_ignore_ascii_case(user)).map(|u| u.name.clone()) else {
		println!("Not tracking '{target}'.");
		return Ok(false);
	};
	exclude_repo(config, archive_root, &owner, target, delete_dir || yes).await
}

/// Removes a single repo under the fully tracked account `owner` by excluding it from syncs, then
/// offers to delete its local copy. Returns `true` if the config changed.
async fn exclude_repo(
	config: &mut Config,
	archive_root: &Path,
	owner: &str,
	target: &str,
	delete: bool,
) -> Result<bool> {
	let (_, name) = parse_repo_arg(target)?;
	let local_dir = find_dir_ignoring_case(&archive_root.join(owner), name)?;
	// Prefer names we already know over a GitHub lookup, so repos deleted upstream can still be removed.
	let full_name = if let Some(dir) = &local_dir {
		format!("{owner}/{}", dir.file_name().map_or_else(|| name.into(), |n| n.to_string_lossy()))
	} else if let Some(existing) = config.excluded.iter().find(|r| r.eq_ignore_ascii_case(target)) {
		existing.clone()
	} else {
		let Ok(repo) = config.build_client()?.repos(owner, name).get().await else {
			println!("'{target}' does not exist on GitHub.");
			return Ok(false);
		};
		repo.full_name.unwrap_or_else(|| format!("{owner}/{name}"))
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

/// GitLab counterpart of `exclude_repo`: removes one project (`host/namespace/project`) under a
/// fully tracked GitLab namespace.
async fn exclude_gitlab_project(
	config: &mut Config,
	archive_root: &Path,
	host: &str,
	path: &str,
	target: &str,
	delete: bool,
) -> Result<bool> {
	let mut dir = archive_root.to_path_buf();
	dir.extend(target.split('/'));
	let local_dir = dir.is_dir().then_some(dir);
	// As for GitHub, prefer names we already know so projects deleted upstream can still be removed.
	let full_name = if local_dir.is_some() {
		target.to_string()
	} else if let Some(existing) = config.excluded.iter().find(|r| r.eq_ignore_ascii_case(target)) {
		existing.clone()
	} else {
		let client = gitlab::GitLabClient::new(host, config.gitlab_token(host))?;
		match client.fetch_project(path).await {
			Ok(project) => format!("{host}/{}", project.path_with_namespace),
			Err(e) => {
				println!("Could not remove '{target}': {e:#}.");
				return Ok(false);
			}
		}
	};
	apply_exclusion(config, &full_name, local_dir, delete)
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

	#[test]
	fn parse_repo_arg_valid() {
		assert_eq!(parse_repo_arg("alice/my-repo").unwrap(), ("alice", "my-repo"));
	}

	#[test]
	fn parse_repo_arg_rejects_malformed() {
		for bad in ["noslash", "a/b/c", "/repo", "user/"] {
			assert!(parse_repo_arg(bad).is_err(), "{bad} should be rejected");
		}
	}

	#[tokio::test]
	async fn exclude_repo_uses_local_casing_and_deletes() {
		let root = temp_dir();
		fs::create_dir_all(root.join("Alice").join("BigRepo")).unwrap();
		let mut config = Config::default();
		config.add_user("Alice", false, false, None);
		assert!(exclude_repo(&mut config, &root, "Alice", "alice/bigrepo", true).await.unwrap());
		assert!(config.excluded.contains("Alice/BigRepo"));
		assert!(!root.join("Alice").join("BigRepo").exists());
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn exclude_repo_already_excluded_is_unchanged() {
		let root = temp_dir();
		let mut config = Config::default();
		config.add_user("alice", false, false, None);
		config.exclude_repo("alice/big");
		assert!(!exclude_repo(&mut config, &root, "alice", "alice/big", true).await.unwrap());
		assert_eq!(config.excluded.len(), 1);
		fs::remove_dir_all(&root).unwrap();
	}

	fn gitlab_user(name: &str) -> TrackedUser {
		TrackedUser { host: Some("gitlab.example.com".to_string()), ..TrackedUser::with_options(name, false, false) }
	}

	#[tokio::test]
	async fn remove_gitlab_excludes_project_under_tracked_group() {
		let root = temp_dir();
		let project_dir = root.join("gitlab.example.com").join("grp").join("sub").join("big");
		fs::create_dir_all(&project_dir).unwrap();
		let mut config = Config::default();
		config.track.push(gitlab_user("grp"));
		let target = "gitlab.example.com/grp/sub/big";
		let changed =
			remove_gitlab(&mut config, &root, "gitlab.example.com", "grp/sub/big", target, true, true).await.unwrap();
		assert!(changed);
		assert!(config.is_excluded(target));
		assert_eq!(config.track.len(), 1, "the group itself stays tracked");
		assert!(!project_dir.exists());
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn remove_gitlab_namespace_clears_its_exclusions_only() {
		let root = temp_dir();
		let mut config = Config::default();
		config.track.push(gitlab_user("grp"));
		config.exclude_repo("gitlab.example.com/grp/big");
		config.exclude_repo("gitlab.example.com/other/big");
		config.exclude_repo("alice/big");
		remove_gitlab(&mut config, &root, "gitlab.example.com", "grp", "gitlab.example.com/grp", false, true)
			.await
			.unwrap();
		assert!(config.track.is_empty());
		assert!(!config.is_excluded("gitlab.example.com/grp/big"));
		assert!(config.is_excluded("gitlab.example.com/other/big"));
		assert!(config.is_excluded("alice/big"));
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn remove_repo_ignores_untracked_owner() {
		let root = temp_dir();
		let mut config = Config::default();
		assert!(!remove_repo(&mut config, &root, "bob/repo", true, true).await.unwrap());
		assert!(config.excluded.is_empty());
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn remove_repo_unpins_pinned_repo_instead_of_excluding() {
		let root = temp_dir();
		let mut config = Config::default();
		config.pin_repo_with_options("bob/repo", None, None);
		assert!(remove_repo(&mut config, &root, "bob/repo", false, true).await.unwrap());
		assert!(!config.is_pinned("bob/repo"));
		assert!(config.excluded.is_empty());
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
		let pins_removed = config.remove_pins_for_user("alice");
		assert_eq!(pins_removed.len(), 2);
		assert!(config.is_pinned("bob/baz"));
	}
}
