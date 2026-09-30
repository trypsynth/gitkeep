use std::{
	collections::{BTreeMap, HashMap, HashSet},
	fs, mem,
	path::PathBuf,
};

use anyhow::{Context, Result, bail};
use dirs::home_dir;
use octocrab::{Octocrab, OctocrabBuilder};
use serde::{Deserialize, Serialize};
use toml::{from_str, to_string_pretty};

mod account;
mod state;

pub use self::{
	account::{HostConfig, PinnedRepo, TrackedAccount},
	state::State,
};
use crate::forge::{Forge, ForgeKind, GitHub, GitLab, split_host};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
	pub token: Option<String>,
	pub archive_dir: Option<String>,
	#[serde(default)]
	pub use_ssh: bool,
	/// Global default for whether to recurse into submodules on clone/pull. Overridden
	/// per-account or per-pin by `TrackedAccount.submodules` / `PinnedRepo.submodules`.
	#[serde(default)]
	pub submodules: bool,
	/// Global default for whether `add` should skip cloning immediately after adding.
	/// Overridden per-invocation by `--sync` / `--no-sync`.
	#[serde(default)]
	pub no_sync: bool,
	#[serde(default)]
	pub track: Vec<TrackedAccount>,
	/// Self-hosted forges, keyed by host (e.g. "gitlab.com"). github.com is never listed; it's the
	/// default and uses `token`.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub hosts: BTreeMap<String, HostConfig>,
	/// Pre-`hosts` per-host GitLab tokens. Only read, and folded into `hosts` on load.
	#[serde(default, skip_serializing)]
	gitlab_tokens: HashMap<String, String>,
	/// Repos under a fully tracked account that were removed with `gitkeep remove user/repo` and
	/// are left out of syncs. Read from the pre-0.3.0 `skipped` key too.
	#[serde(default, alias = "skipped", skip_serializing_if = "HashSet::is_empty")]
	pub excluded: HashSet<String>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub pinned: Vec<PinnedRepo>,
}

impl Config {
	pub fn path() -> Result<PathBuf> {
		let home = home_dir().context("Could not find your home directory")?;
		Ok(home.join(".gitkeep.toml"))
	}

	pub fn load() -> Result<Self> {
		let path = Self::path()?;
		if !path.exists() {
			return Ok(Self::default());
		}
		let raw =
			fs::read_to_string(&path).with_context(|| format!("Could not read config from {}", path.display()))?;
		let mut config: Self =
			from_str(&raw).with_context(|| format!("Config at {} is not valid TOML", path.display()))?;
		config.migrate_legacy_hosts();
		Ok(config)
	}

	/// Before `[hosts]` existed, GitLab was the only other forge: tokens lived in `gitlab_tokens` and
	/// any host-qualified account or pin was on GitLab. Record those hosts explicitly.
	fn migrate_legacy_hosts(&mut self) {
		for (host, token) in mem::take(&mut self.gitlab_tokens) {
			self.hosts.entry(host).or_insert(HostConfig { kind: ForgeKind::GitLab, token: None }).token = Some(token);
		}
		let used = self
			.track
			.iter()
			.filter_map(|u| u.host.clone())
			.chain(self.pinned.iter().filter_map(|p| split_host(&p.full_name).map(|(h, _)| h.to_string())))
			.collect::<Vec<_>>();
		for host in used {
			self.hosts.entry(host).or_insert(HostConfig { kind: ForgeKind::GitLab, token: None });
		}
	}

	pub fn save(&self) -> Result<()> {
		let path = Self::path()?;
		let raw = to_string_pretty(self).context("Could not serialize config")?;
		fs::write(&path, raw).with_context(|| format!("Could not write config to {}", path.display()))
	}

	pub fn archive_dir(&self) -> Result<PathBuf> {
		if let Some(dir) = &self.archive_dir {
			Ok(PathBuf::from(dir))
		} else {
			let home = home_dir().context("Could not find your home directory")?;
			Ok(home.join("gitkeep"))
		}
	}

	pub fn build_client(&self) -> Result<Octocrab> {
		self.token.as_ref().map_or_else(
			|| {
				println!("Warning: Running in unauthenticated mode. Rate limits will be restricted.");
				OctocrabBuilder::default().build().context("Could not create GitHub client")
			},
			|token| {
				OctocrabBuilder::default()
					.personal_token(token.clone())
					.build()
					.context("Could not create authenticated GitHub client")
			},
		)
	}

	/// Connects to the forge at `host` (`None` for GitHub). The host must already be in `hosts`.
	pub fn forge(&self, host: Option<&str>) -> Result<Forge> {
		let Some(host) = host else {
			return Ok(Forge::GitHub(GitHub::new(self.build_client()?, self.token.is_some())));
		};
		let Some(entry) = self.hosts.get(host) else {
			bail!("{host} isn't a known forge. Add something from it with 'gitkeep add https://{host}/...' first.");
		};
		match entry.kind {
			ForgeKind::GitLab => Ok(Forge::GitLab(GitLab::new(host, entry.token.as_deref())?)),
		}
	}

	pub fn add_account(
		&mut self,
		host: Option<&str>,
		user: &str,
		forks: bool,
		frozen: bool,
		submodules: Option<bool>,
	) -> bool {
		let display = host.map_or_else(|| user.to_string(), |h| format!("{h}/{user}"));
		let changed = if let Some(entry) =
			self.track.iter_mut().find(|u| u.name.eq_ignore_ascii_case(user) && u.host.as_deref() == host)
		{
			let canonical_changed = if entry.name == user {
				false
			} else {
				entry.name = user.to_string();
				true
			};
			let mut local_changed = if forks && !entry.forks {
				entry.forks = true;
				println!("Forks enabled for {display}.");
				true
			} else {
				false
			};
			if frozen && !entry.frozen {
				entry.frozen = true;
				println!("Account frozen for {display}. Updates will be skipped.");
				local_changed = true;
			} else if !frozen && entry.frozen {
				entry.frozen = false;
				println!("Account unfrozen for {display}. Updates will be included.");
				local_changed = true;
			}
			if let Some(submodules) = submodules
				&& entry.submodules != Some(submodules)
			{
				entry.submodules = Some(submodules);
				println!("Submodules {} for {display}.", if submodules { "enabled" } else { "disabled" });
				local_changed = true;
			}
			if !local_changed && !canonical_changed {
				println!("Already tracking {display}.");
			}
			local_changed || canonical_changed
		} else {
			let mut entry = TrackedAccount::with_options(user, forks, frozen);
			entry.submodules = submodules;
			entry.host = host.map(ToString::to_string);
			println!(
				"Now tracking {}{}{}",
				display,
				if forks { " (forks included)" } else { "" },
				if frozen { " (frozen)" } else { "" }
			);
			self.track.push(entry);
			true
		};
		if changed {
			self.sort_accounts();
		}
		changed
	}

	/// Stops tracking the account `user` on `host` (`None` for GitHub).
	pub fn remove_account(&mut self, host: Option<&str>, user: &str) -> bool {
		let before = self.track.len();
		self.track.retain(|u| !(u.host.as_deref() == host && u.name.eq_ignore_ascii_case(user)));
		let display = host.map_or_else(|| user.to_string(), |h| format!("{h}/{user}"));
		if self.track.len() < before {
			println!("Stopped tracking {display}.");
			true
		} else {
			println!("Not tracking {display}.");
			false
		}
	}

	pub fn sort_accounts(&mut self) {
		self.track.sort_by_key(|a| a.name.to_lowercase());
	}

	/// Returns `true` if this is a new exclusion, `false` if the repo was already excluded.
	pub fn exclude_repo(&mut self, full_name: &str) -> bool {
		if self.is_excluded(full_name) {
			return false;
		}
		self.excluded.insert(full_name.to_string())
	}

	/// Removes an exclusion (case-insensitively) and returns the stored name, or `None` if the
	/// repo wasn't excluded.
	pub fn include_repo(&mut self, full_name: &str) -> Option<String> {
		let stored = self.excluded.iter().find(|r| r.eq_ignore_ascii_case(full_name))?.clone();
		self.excluded.remove(&stored);
		Some(stored)
	}

	pub fn is_excluded(&self, full_name: &str) -> bool {
		self.excluded.iter().any(|r| r.eq_ignore_ascii_case(full_name))
	}

	/// Drops every exclusion that `tracked`'s sync would include, e.g. once that account is removed.
	pub fn remove_exclusions_covered(&mut self, tracked: &TrackedAccount) {
		self.excluded.retain(|r| !tracked.covers(r));
	}

	/// Pins a repo, optionally recording its stable GitHub id (used to re-resolve it after a
	/// rename) and a per-pin submodules override. Returns `true` if this is a new pin, `false`
	/// if already pinned.
	pub fn pin_repo_with_options(&mut self, full_name: &str, id: Option<u64>, submodules: Option<bool>) -> bool {
		if self.is_pinned(full_name) {
			return false;
		}
		self.pinned.push(PinnedRepo { full_name: full_name.to_string(), id, submodules });
		true
	}

	/// Returns `true` if the repo was pinned and is now removed, `false` if it wasn't pinned.
	/// Unpins a repo (case-insensitively) and returns its stored name, or `None` if it wasn't pinned.
	pub fn unpin_repo(&mut self, full_name: &str) -> Option<String> {
		let index = self.pinned.iter().position(|p| p.full_name.eq_ignore_ascii_case(full_name))?;
		Some(self.pinned.remove(index).full_name)
	}

	pub fn is_pinned(&self, full_name: &str) -> bool {
		self.pinned.iter().any(|p| p.full_name == full_name)
	}

	/// Returns the stored GitHub id for a pinned repo, if any.
	pub fn pinned_id(&self, full_name: &str) -> Option<u64> {
		self.pinned.iter().find(|p| p.full_name == full_name)?.id
	}

	/// Returns the per-pin submodules override, if any (`None` means inherit the global default).
	pub fn pinned_submodules(&self, full_name: &str) -> Option<bool> {
		self.pinned.iter().find(|p| p.full_name == full_name)?.submodules
	}

	/// Updates a pin's `full_name` in place (used when the owner or repo has been renamed).
	/// Returns `true` if `old_full_name` was found and renamed.
	pub fn rename_pin(&mut self, old_full_name: &str, new_full_name: &str) -> bool {
		if let Some(pin) = self.pinned.iter_mut().find(|p| p.full_name == old_full_name) {
			pin.full_name = new_full_name.to_string();
			true
		} else {
			false
		}
	}

	/// Removes all pinned repos already covered by `tracked` and returns their full names.
	pub fn remove_pins_covered(&mut self, tracked: &TrackedAccount) -> Vec<String> {
		let to_remove: Vec<String> =
			self.pinned.iter().filter(|p| tracked.covers(&p.full_name)).map(|p| p.full_name.clone()).collect();
		self.pinned.retain(|p| !to_remove.contains(&p.full_name));
		to_remove
	}
}

#[cfg(test)]
mod tests {
	use toml::to_string;

	use super::*;

	#[test]
	fn exclude_repo_marks_as_excluded() {
		let mut config = Config::default();
		config.exclude_repo("user/repo");
		assert!(config.is_excluded("user/repo"));
	}

	#[test]
	fn exclude_repo_returns_true_for_new_exclusion() {
		let mut config = Config::default();
		assert!(config.exclude_repo("user/repo"));
	}

	#[test]
	fn exclude_repo_returns_false_for_duplicate_in_any_case() {
		let mut config = Config::default();
		config.exclude_repo("user/repo");
		assert!(!config.exclude_repo("User/Repo"));
		assert_eq!(config.excluded.len(), 1);
	}

	#[test]
	fn exclude_repo_does_not_affect_other_repos() {
		let mut config = Config::default();
		config.exclude_repo("user/repo");
		assert!(!config.is_excluded("user/other"));
	}

	#[test]
	fn is_excluded_ignores_case() {
		let mut config = Config::default();
		config.exclude_repo("User/Repo");
		assert!(config.is_excluded("user/repo"));
	}

	#[test]
	fn include_repo_returns_stored_name() {
		let mut config = Config::default();
		config.exclude_repo("User/Repo");
		assert_eq!(config.include_repo("user/repo").as_deref(), Some("User/Repo"));
		assert!(!config.is_excluded("User/Repo"));
	}

	#[test]
	fn include_repo_returns_none_when_not_excluded() {
		let mut config = Config::default();
		assert!(config.include_repo("user/repo").is_none());
	}

	#[test]
	fn remove_exclusions_covered_only_touches_that_user() {
		let mut config = Config::default();
		config.exclude_repo("Alice/a");
		config.exclude_repo("alice/b");
		config.exclude_repo("bob/c");
		config.remove_exclusions_covered(&TrackedAccount::with_options("alice", false, false));
		assert!(!config.is_excluded("alice/a"));
		assert!(!config.is_excluded("alice/b"));
		assert!(config.is_excluded("bob/c"));
	}

	#[test]
	fn legacy_skipped_key_loads_as_excluded() {
		let config: Config = from_str("skipped = [\"user/repo\"]").unwrap();
		assert!(config.is_excluded("user/repo"));
		assert!(to_string_pretty(&config).unwrap().contains("excluded"));
	}

	#[test]
	fn config_pin_repo_marks_as_pinned() {
		let mut config = Config::default();
		config.pin_repo_with_options("user/repo", None, None);
		assert!(config.is_pinned("user/repo"));
	}

	#[test]
	fn config_pin_repo_returns_true_for_new_pin() {
		let mut config = Config::default();
		assert!(config.pin_repo_with_options("user/repo", None, None));
	}

	#[test]
	fn config_pin_repo_returns_false_for_duplicate() {
		let mut config = Config::default();
		config.pin_repo_with_options("user/repo", None, None);
		assert!(!config.pin_repo_with_options("user/repo", None, None));
	}

	#[test]
	fn config_unpin_repo_returns_stored_name_ignoring_case() {
		let mut config = Config::default();
		config.pin_repo_with_options("user/repo", None, None);
		assert_eq!(config.unpin_repo("User/Repo").as_deref(), Some("user/repo"));
	}

	#[test]
	fn config_unpin_repo_returns_none_when_not_pinned() {
		let mut config = Config::default();
		assert!(config.unpin_repo("user/repo").is_none());
	}

	#[test]
	fn config_is_pinned_false_for_unknown() {
		let config = Config::default();
		assert!(!config.is_pinned("user/repo"));
	}

	#[test]
	fn add_account_sets_submodules_override_on_new_entry() {
		let mut config = Config::default();
		config.add_account(None, "alice", false, false, Some(true));
		assert_eq!(config.track[0].submodules, Some(true));
	}

	#[test]
	fn add_account_leaves_submodules_unset_when_not_specified() {
		let mut config = Config::default();
		config.add_account(None, "alice", false, false, None);
		assert_eq!(config.track[0].submodules, None);
	}

	#[test]
	fn add_account_updates_submodules_override_on_existing_entry() {
		let mut config = Config::default();
		config.add_account(None, "alice", false, false, None);
		let changed = config.add_account(None, "alice", false, false, Some(false));
		assert!(changed);
		assert_eq!(config.track[0].submodules, Some(false));
	}

	#[test]
	fn add_account_no_change_when_submodules_override_already_set() {
		let mut config = Config::default();
		config.add_account(None, "alice", false, false, Some(true));
		let changed = config.add_account(None, "alice", false, false, Some(true));
		assert!(!changed);
	}

	#[test]
	fn pin_repo_with_options_stores_submodules_override() {
		let mut config = Config::default();
		config.pin_repo_with_options("alice/repo", None, Some(true));
		assert_eq!(config.pinned_submodules("alice/repo"), Some(true));
	}

	#[test]
	fn pin_repo_with_options_leaves_submodules_unset_when_not_specified() {
		let mut config = Config::default();
		config.pin_repo_with_options("alice/repo", None, None);
		assert_eq!(config.pinned_submodules("alice/repo"), None);
	}

	#[test]
	fn pinned_submodules_none_for_unknown_repo() {
		let config = Config::default();
		assert_eq!(config.pinned_submodules("alice/repo"), None);
	}

	#[test]
	fn config_loads_legacy_pinned_string_array_with_no_submodules_override() {
		let raw = r#"pinned = ["alice/repo"]"#;
		let config: Config = from_str(raw).unwrap();
		assert_eq!(config.pinned_submodules("alice/repo"), None);
	}

	#[test]
	fn config_loads_new_pinned_table_with_submodules_override() {
		let raw = r#"
			[[pinned]]
			full_name = "alice/repo"
			submodules = true
		"#;
		let config: Config = from_str(raw).unwrap();
		assert_eq!(config.pinned_submodules("alice/repo"), Some(true));
	}

	#[test]
	fn config_submodules_defaults_to_false_for_legacy_toml() {
		let config: Config = from_str(r#"archive_dir = "/tmp/x""#).unwrap();
		assert!(!config.submodules);
	}

	#[test]
	fn config_loads_legacy_pinned_string_array() {
		let raw = r#"pinned = ["alice/repo", "bob/other"]"#;
		let config: Config = from_str(raw).expect("legacy pinned string array should still deserialize");
		assert!(config.is_pinned("alice/repo"));
		assert!(config.is_pinned("bob/other"));
		assert_eq!(config.pinned_id("alice/repo"), None);
	}

	#[test]
	fn config_loads_new_pinned_table_array() {
		let raw = r#"
			[[pinned]]
			full_name = "alice/repo"
			id = 42
		"#;
		let config: Config = from_str(raw).expect("new pinned table array should deserialize");
		assert!(config.is_pinned("alice/repo"));
		assert_eq!(config.pinned_id("alice/repo"), Some(42));
	}

	#[test]
	fn config_remove_pins_covered_removes_matching() {
		let mut config = Config::default();
		config.pin_repo_with_options("alice/foo", None, None);
		config.pin_repo_with_options("alice/bar", None, None);
		config.pin_repo_with_options("bob/baz", None, None);
		let removed = config.remove_pins_covered(&TrackedAccount::with_options("alice", false, false));
		assert_eq!(removed.len(), 2);
		assert!(!config.is_pinned("alice/foo"));
		assert!(!config.is_pinned("alice/bar"));
		assert!(config.is_pinned("bob/baz"));
	}

	#[test]
	fn config_remove_pins_covered_case_insensitive() {
		let mut config = Config::default();
		config.pin_repo_with_options("Alice/foo", None, None);
		let removed = config.remove_pins_covered(&TrackedAccount::with_options("alice", false, false));
		assert_eq!(removed.len(), 1);
		assert!(!config.is_pinned("Alice/foo"));
	}

	#[test]
	fn config_remove_pins_covered_returns_empty_when_none() {
		let mut config = Config::default();
		config.pin_repo_with_options("bob/baz", None, None);
		let removed = config.remove_pins_covered(&TrackedAccount::with_options("alice", false, false));
		assert!(removed.is_empty());
	}

	#[test]
	fn pin_repo_stores_id() {
		let mut config = Config::default();
		config.pin_repo_with_options("alice/repo", Some(7), None);
		assert_eq!(config.pinned_id("alice/repo"), Some(7));
	}

	#[test]
	fn pin_repo_without_id_stores_none() {
		let mut config = Config::default();
		config.pin_repo_with_options("alice/repo", None, None);
		assert_eq!(config.pinned_id("alice/repo"), None);
	}

	#[test]
	fn pin_repo_with_options_returns_false_for_duplicate_full_name() {
		let mut config = Config::default();
		config.pin_repo_with_options("alice/repo", None, None);
		assert!(!config.pin_repo_with_options("alice/repo", Some(7), None));
	}

	#[test]
	fn rename_pin_updates_full_name_and_keeps_id() {
		let mut config = Config::default();
		config.pin_repo_with_options("alice/repo", Some(7), None);
		assert!(config.rename_pin("alice/repo", "bob/repo"));
		assert!(config.is_pinned("bob/repo"));
		assert!(!config.is_pinned("alice/repo"));
		assert_eq!(config.pinned_id("bob/repo"), Some(7));
	}

	#[test]
	fn config_without_gitlab_omits_new_fields_on_save() {
		let mut config = Config::default();
		config.add_account(None, "alice", false, false, None);
		let raw = to_string(&config).unwrap();
		assert!(!raw.contains("gitlab_tokens"), "got: {raw}");
		assert!(!raw.contains("host"), "got: {raw}");
	}

	#[test]
	fn legacy_gitlab_tokens_migrate_into_hosts() {
		let mut config: Config = from_str(
			r#"
			[gitlab_tokens]
			"gitlab.example.com" = "glpat-secret"
			"#,
		)
		.unwrap();
		config.migrate_legacy_hosts();
		let host = &config.hosts["gitlab.example.com"];
		assert_eq!(host.kind, ForgeKind::GitLab);
		assert_eq!(host.token.as_deref(), Some("glpat-secret"));
		let raw = to_string_pretty(&config).unwrap();
		assert!(!raw.contains("gitlab_tokens"), "got: {raw}");
		assert!(raw.contains("[hosts.\"gitlab.example.com\"]"), "got: {raw}");
	}

	#[test]
	fn legacy_host_qualified_entries_are_assumed_gitlab() {
		let mut config = Config::default();
		config.add_account(Some("git.example.org"), "grp", false, false, None);
		config.pin_repo_with_options("code.example.net/team/proj", None, None);
		config.migrate_legacy_hosts();
		assert_eq!(config.hosts["git.example.org"].kind, ForgeKind::GitLab);
		assert_eq!(config.hosts["code.example.net"].kind, ForgeKind::GitLab);
		assert!(config.hosts["git.example.org"].token.is_none());
	}

	#[test]
	fn migration_keeps_existing_host_kinds() {
		let mut config = Config::default();
		config
			.hosts
			.insert("git.example.org".to_string(), HostConfig { kind: ForgeKind::GitLab, token: Some("t".into()) });
		config.add_account(Some("git.example.org"), "grp", false, false, None);
		config.migrate_legacy_hosts();
		assert_eq!(config.hosts["git.example.org"].token.as_deref(), Some("t"));
	}

	#[test]
	fn forge_for_unknown_host_is_an_error() {
		let config = Config::default();
		let err = config.forge(Some("git.example.org")).err().unwrap();
		assert!(err.to_string().contains("isn't a known forge"), "got: {err}");
	}

	#[test]
	fn add_account_on_tracks_same_name_on_different_hosts_separately() {
		let mut config = Config::default();
		config.add_account(None, "alice", false, false, None);
		assert!(config.add_account(Some("gitlab.example.com"), "alice", false, false, None));
		assert_eq!(config.track.len(), 2);
	}

	#[test]
	fn add_account_on_is_idempotent_per_host() {
		let mut config = Config::default();
		config.add_account(Some("gitlab.example.com"), "alice", false, false, None);
		assert!(!config.add_account(Some("gitlab.example.com"), "alice", false, false, None));
	}

	#[test]
	fn remove_pins_covered_removes_gitlab_pins() {
		let mut config = Config::default();
		config.pin_repo_with_options("gitlab.example.com/grp/proj", None, None);
		config.pin_repo_with_options("gitlab.example.com/other/proj", None, None);
		let tracked = TrackedAccount {
			host: Some("gitlab.example.com".to_string()),
			..TrackedAccount::with_options("grp", false, false)
		};
		let removed = config.remove_pins_covered(&tracked);
		assert_eq!(removed, vec!["gitlab.example.com/grp/proj".to_string()]);
		assert!(config.is_pinned("gitlab.example.com/other/proj"));
	}

	#[test]
	fn rename_pin_returns_false_when_source_missing() {
		let mut config = Config::default();
		assert!(!config.rename_pin("alice/repo", "bob/repo"));
	}

	#[test]
	fn add_account_removes_pins_for_that_user() {
		let mut config = Config::default();
		config.pin_repo_with_options("alice/foo", None, None);
		config.pin_repo_with_options("alice/bar", None, None);
		config.pin_repo_with_options("bob/baz", None, None);
		config.add_account(None, "alice", false, false, None);
		let pins_removed = config.remove_pins_covered(&TrackedAccount::with_options("alice", false, false));
		assert_eq!(pins_removed.len(), 2);
		assert!(config.is_pinned("bob/baz"));
	}
}
