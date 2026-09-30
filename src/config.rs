use std::{
	collections::{BTreeMap, HashMap, HashSet},
	fs, mem,
	path::PathBuf,
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use dirs::home_dir;
use octocrab::{Octocrab, OctocrabBuilder};
use serde::{Deserialize, Deserializer, Serialize};
use toml::{from_str, to_string_pretty};

use crate::forge::{Forge, ForgeKind, GitHub, GitLab, split_host};

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_false(v: &bool) -> bool {
	!*v
}

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

/// A self-hosted forge: which software it runs and, optionally, a token for private repos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostConfig {
	pub kind: ForgeKind,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedAccount {
	pub name: String,
	#[serde(default, skip_serializing_if = "is_false")]
	pub forks: bool,
	#[serde(default, skip_serializing_if = "is_false")]
	pub frozen: bool,
	/// Stable GitHub account id, used to re-resolve the account if it gets renamed.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<u64>,
	/// Overrides the global `submodules` default for this account. `None` inherits it.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub submodules: Option<bool>,
	/// Forge host this account lives on (e.g. "gitlab.example.com"), described in `Config::hosts`.
	/// `None` means GitHub.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub host: Option<String>,
}

impl TrackedAccount {
	pub fn with_options(name: impl Into<String>, forks: bool, frozen: bool) -> Self {
		Self { name: name.into(), forks, frozen, id: None, submodules: None, host: None }
	}

	pub fn display_name(&self) -> String {
		self.host.as_ref().map_or_else(|| self.name.clone(), |h| format!("{h}/{}", self.name))
	}

	/// True when this tracked account's sync already includes `full_name` (a pin key),
	/// so an individual pin would be redundant. For GitLab entries a subgroup project is
	/// covered too, since group syncs include subgroups.
	pub fn covers(&self, full_name: &str) -> bool {
		match (&self.host, split_host(full_name)) {
			(Some(h), Some((host, path))) => {
				h == host
					&& path.rsplit_once('/').is_some_and(|(owner, _)| {
						owner.eq_ignore_ascii_case(&self.name)
							|| owner.to_lowercase().starts_with(&format!("{}/", self.name.to_lowercase()))
					})
			}
			(None, None) => full_name.split_once('/').is_some_and(|(u, _)| u.eq_ignore_ascii_case(&self.name)),
			_ => false,
		}
	}
}

#[derive(Debug, Clone, Serialize)]
pub struct PinnedRepo {
	pub full_name: String,
	/// Stable GitHub repository id, used to re-resolve the repo if it or its owner gets renamed.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<u64>,
	/// Overrides the global `submodules` default for this pin. `None` inherits it.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub submodules: Option<bool>,
}

// Accepts both the legacy bare-string form (`pinned = ["user/repo"]`) and the current
// table form (`[[pinned]] full_name = "user/repo" id = 42`), so existing configs keep
// loading after this field's on-disk shape changed.
impl<'de> Deserialize<'de> for PinnedRepo {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		#[derive(Deserialize)]
		#[serde(untagged)]
		enum Repr {
			Legacy(String),
			Full {
				full_name: String,
				#[serde(default)]
				id: Option<u64>,
				#[serde(default)]
				submodules: Option<bool>,
			},
		}
		Ok(match Repr::deserialize(deserializer)? {
			Repr::Legacy(full_name) => Self { full_name, id: None, submodules: None },
			Repr::Full { full_name, id, submodules } => Self { full_name, id, submodules },
		})
	}
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

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
	#[serde(default)]
	pub repos: HashMap<String, RepoState>,
	#[serde(default, skip_serializing)]
	pub skipped: HashSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoState {
	pub last_synced_at: DateTime<Utc>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub pushed_at: Option<DateTime<Utc>>,
	/// Stable GitHub repo id, used to detect an `owner/name` being reused by a different
	/// repo (e.g. deleted and recreated), which should force a re-clone rather than a pull.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<u64>,
}

impl State {
	pub fn path() -> Result<PathBuf> {
		let home = home_dir().context("Could not find your home directory")?;
		Ok(home.join(".gitkeep_state.toml"))
	}

	pub fn load() -> Result<Self> {
		let path = Self::path()?;
		if !path.exists() {
			return Ok(Self::default());
		}
		let raw = fs::read_to_string(&path).with_context(|| format!("Could not read state from {}", path.display()))?;
		Ok(from_str(&raw).unwrap_or_default())
	}

	pub fn save(&self) -> Result<()> {
		let path = Self::path()?;
		let raw = to_string_pretty(self).context("Could not serialize state")?;
		fs::write(&path, raw).with_context(|| format!("Could not write state to {}", path.display()))
	}

	pub fn mark_synced(&mut self, full_name: &str, pushed_at: Option<DateTime<Utc>>, id: u64) {
		self.repos.insert(full_name.to_string(), RepoState { last_synced_at: Utc::now(), pushed_at, id: Some(id) });
	}

	pub fn drain_legacy_skipped(&mut self) -> HashSet<String> {
		mem::take(&mut self.skipped)
	}
}

#[cfg(test)]
mod tests {
	use toml::{Value, to_string};

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
	fn state_mark_synced_stores_pushed_at() {
		let mut state = State::default();
		let t = Utc::now();
		state.mark_synced("user/repo", Some(t), 1);
		let stored = state.repos["user/repo"].pushed_at;
		assert!(stored.is_some());
	}

	#[test]
	fn state_mark_synced_stores_none_pushed_at() {
		let mut state = State::default();
		state.mark_synced("user/repo", None, 1);
		assert!(state.repos["user/repo"].pushed_at.is_none());
	}

	#[test]
	fn state_mark_synced_stores_id() {
		let mut state = State::default();
		state.mark_synced("user/repo", None, 42);
		assert_eq!(state.repos["user/repo"].id, Some(42));
	}

	#[test]
	fn state_drain_legacy_skipped_moves_entries() {
		let mut state = State::default();
		state.skipped.insert("user/repo".to_string());
		let drained = state.drain_legacy_skipped();
		assert!(drained.contains("user/repo"));
	}

	#[test]
	fn state_drain_legacy_skipped_empties_state() {
		let mut state = State::default();
		state.skipped.insert("user/repo".to_string());
		state.drain_legacy_skipped();
		assert!(state.skipped.is_empty());
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
	fn pinned_repo_deserializes_legacy_bare_string() {
		let repo: PinnedRepo = Value::String("alice/repo".to_string()).try_into().unwrap();
		assert_eq!(repo.full_name, "alice/repo");
		assert_eq!(repo.id, None);
	}

	#[test]
	fn tracked_user_submodules_defaults_to_none_for_legacy_toml() {
		let user: TrackedAccount = from_str(r#"name = "alice""#).unwrap();
		assert_eq!(user.submodules, None);
	}

	#[test]
	fn tracked_user_submodules_round_trips() {
		let user = TrackedAccount { submodules: Some(true), ..TrackedAccount::with_options("alice", false, false) };
		let raw = to_string(&user).unwrap();
		let back: TrackedAccount = from_str(&raw).unwrap();
		assert_eq!(back.submodules, Some(true));
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
	fn tracked_user_id_defaults_to_none_when_deserializing_legacy_toml() {
		let user: TrackedAccount = from_str(r#"name = "alice""#).unwrap();
		assert_eq!(user.id, None);
	}

	#[test]
	fn tracked_user_id_round_trips() {
		let user = TrackedAccount { id: Some(42), ..TrackedAccount::with_options("alice", false, false) };
		let raw = to_string(&user).unwrap();
		let back: TrackedAccount = from_str(&raw).unwrap();
		assert_eq!(back.id, Some(42));
	}

	#[test]
	fn tracked_user_id_omitted_from_toml_when_none() {
		let user = TrackedAccount::with_options("alice", false, false);
		let raw = to_string(&user).unwrap();
		assert!(!raw.contains("id"), "got: {raw}");
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
	fn tracked_user_host_defaults_to_none_for_legacy_toml() {
		let user: TrackedAccount = from_str(r#"name = "alice""#).unwrap();
		assert_eq!(user.host, None);
	}

	#[test]
	fn tracked_user_host_round_trips() {
		let user = TrackedAccount {
			host: Some("gitlab.example.com".to_string()),
			..TrackedAccount::with_options("grp", false, false)
		};
		let raw = to_string(&user).unwrap();
		let back: TrackedAccount = from_str(&raw).unwrap();
		assert_eq!(back.host.as_deref(), Some("gitlab.example.com"));
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
	fn display_name_includes_host_for_gitlab() {
		let user = TrackedAccount {
			host: Some("gitlab.example.com".to_string()),
			..TrackedAccount::with_options("grp", false, false)
		};
		assert_eq!(user.display_name(), "gitlab.example.com/grp");
	}

	#[test]
	fn covers_github_pin_by_owner() {
		let user = TrackedAccount::with_options("Alice", false, false);
		assert!(user.covers("alice/repo"));
		assert!(!user.covers("bob/repo"));
	}

	#[test]
	fn covers_rejects_cross_provider() {
		let github = TrackedAccount::with_options("alice", false, false);
		assert!(!github.covers("gitlab.example.com/alice/repo"));
		let gitlab = TrackedAccount {
			host: Some("gitlab.example.com".to_string()),
			..TrackedAccount::with_options("alice", false, false)
		};
		assert!(!gitlab.covers("alice/repo"));
	}

	#[test]
	fn covers_gitlab_pin_including_subgroups() {
		let user = TrackedAccount {
			host: Some("gitlab.example.com".to_string()),
			..TrackedAccount::with_options("grp", false, false)
		};
		assert!(user.covers("gitlab.example.com/grp/proj"));
		assert!(user.covers("gitlab.example.com/grp/sub/proj"));
		assert!(!user.covers("gitlab.example.com/other/proj"));
		assert!(!user.covers("gitlab.other.com/grp/proj"));
	}

	#[test]
	fn covers_gitlab_does_not_match_sibling_prefix() {
		let user = TrackedAccount {
			host: Some("gitlab.example.com".to_string()),
			..TrackedAccount::with_options("grp", false, false)
		};
		assert!(!user.covers("gitlab.example.com/grpx/proj"));
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
}
