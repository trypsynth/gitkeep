use std::{
	collections::{HashMap, HashSet},
	fs, mem,
	path::PathBuf,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use dirs::home_dir;
use octocrab::{Octocrab, OctocrabBuilder};
use serde::{Deserialize, Serialize};
use toml::{from_str, to_string_pretty};

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
	#[serde(default)]
	pub track: Vec<TrackedUser>,
	#[serde(default, skip_serializing_if = "HashSet::is_empty")]
	pub skipped: HashSet<String>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub pinned: Vec<PinnedRepo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedUser {
	pub name: String,
	#[serde(default, skip_serializing_if = "is_false")]
	pub forks: bool,
	#[serde(default, skip_serializing_if = "is_false")]
	pub frozen: bool,
	/// Stable GitHub account id, used to re-resolve the account if it gets renamed.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<u64>,
}

impl TrackedUser {
	pub fn with_options(name: impl Into<String>, forks: bool, frozen: bool) -> Self {
		Self { name: name.into(), forks, frozen, id: None }
	}
}

#[derive(Debug, Clone, Serialize)]
pub struct PinnedRepo {
	pub full_name: String,
	/// Stable GitHub repository id, used to re-resolve the repo if it or its owner gets renamed.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<u64>,
}

// Accepts both the legacy bare-string form (`pinned = ["user/repo"]`) and the current
// table form (`[[pinned]] full_name = "user/repo" id = 42`), so existing configs keep
// loading after this field's on-disk shape changed.
impl<'de> Deserialize<'de> for PinnedRepo {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		#[derive(Deserialize)]
		#[serde(untagged)]
		enum Repr {
			Legacy(String),
			Full {
				full_name: String,
				#[serde(default)]
				id: Option<u64>,
			},
		}
		Ok(match Repr::deserialize(deserializer)? {
			Repr::Legacy(full_name) => Self { full_name, id: None },
			Repr::Full { full_name, id } => Self { full_name, id },
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
		from_str(&raw).with_context(|| format!("Config at {} is not valid TOML", path.display()))
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

	pub fn add_user(&mut self, user: &str, forks: bool, frozen: bool) -> bool {
		let changed = if let Some(entry) = self.track.iter_mut().find(|u| u.name.eq_ignore_ascii_case(user)) {
			let canonical_changed = if entry.name == user {
				false
			} else {
				entry.name = user.to_string();
				true
			};

			let mut local_changed = if forks && !entry.forks {
				entry.forks = true;
				println!("Forks enabled for {user}.");
				true
			} else {
				false
			};

			if frozen && !entry.frozen {
				entry.frozen = true;
				println!("Account frozen for {user}. Updates will be skipped.");
				local_changed = true;
			} else if !frozen && entry.frozen {
				entry.frozen = false;
				println!("Account unfrozen for {user}. Updates will be included.");
				local_changed = true;
			}

			if !local_changed && !canonical_changed {
				println!("Already tracking {user}.");
			}
			local_changed || canonical_changed
		} else {
			let entry = TrackedUser::with_options(user, forks, frozen);
			println!(
				"Now tracking {}{}{}",
				user,
				if forks { " (forks included)" } else { "" },
				if frozen { " (frozen)" } else { "" }
			);
			self.track.push(entry);
			true
		};

		if changed {
			self.sort_users();
		}
		changed
	}

	pub fn remove_user(&mut self, user: &str) -> bool {
		let before = self.track.len();
		self.track.retain(|u| !u.name.eq_ignore_ascii_case(user));
		if self.track.len() < before {
			println!("Stopped tracking {user}.");
			true
		} else {
			println!("Not tracking {user}.");
			false
		}
	}

	pub fn sort_users(&mut self) {
		self.track.sort_by_key(|a| a.name.to_lowercase());
	}

	/// Returns `true` if this is a new skip, `false` if already skipped.
	pub fn skip_repo(&mut self, full_name: &str) -> bool {
		self.skipped.insert(full_name.to_string())
	}

	/// Returns `true` if the repo was skipped and is now removed, `false` if it wasn't skipped.
	pub fn unskip_repo(&mut self, full_name: &str) -> bool {
		self.skipped.remove(full_name)
	}

	pub fn is_skipped(&self, full_name: &str) -> bool {
		self.skipped.contains(full_name)
	}

	/// Pins a repo, optionally recording its stable GitHub id (used to re-resolve it after a
	/// rename). Returns `true` if this is a new pin, `false` if already pinned.
	pub fn pin_repo_with_id(&mut self, full_name: &str, id: Option<u64>) -> bool {
		if self.is_pinned(full_name) {
			return false;
		}
		self.pinned.push(PinnedRepo { full_name: full_name.to_string(), id });
		true
	}

	/// Returns `true` if the repo was pinned and is now removed, `false` if it wasn't pinned.
	pub fn unpin_repo(&mut self, full_name: &str) -> bool {
		let before = self.pinned.len();
		self.pinned.retain(|p| p.full_name != full_name);
		self.pinned.len() < before
	}

	pub fn is_pinned(&self, full_name: &str) -> bool {
		self.pinned.iter().any(|p| p.full_name == full_name)
	}

	/// Returns the stored GitHub id for a pinned repo, if any.
	pub fn pinned_id(&self, full_name: &str) -> Option<u64> {
		self.pinned.iter().find(|p| p.full_name == full_name)?.id
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

	/// Removes all pinned repos owned by `user` (case-insensitive) and returns their full names.
	pub fn remove_pins_for_user(&mut self, user: &str) -> Vec<String> {
		let to_remove: Vec<String> = self
			.pinned
			.iter()
			.filter(|p| p.full_name.split_once('/').is_some_and(|(u, _)| u.eq_ignore_ascii_case(user)))
			.map(|p| p.full_name.clone())
			.collect();
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

	pub fn mark_synced(&mut self, full_name: &str, pushed_at: Option<DateTime<Utc>>) {
		self.repos.insert(full_name.to_string(), RepoState { last_synced_at: Utc::now(), pushed_at });
	}

	pub fn drain_legacy_skipped(&mut self) -> HashSet<String> {
		mem::take(&mut self.skipped)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn config_skip_repo_marks_as_skipped() {
		let mut config = Config::default();
		config.skip_repo("user/repo");
		assert!(config.is_skipped("user/repo"));
	}

	#[test]
	fn config_skip_repo_returns_true_for_new_skip() {
		let mut config = Config::default();
		assert!(config.skip_repo("user/repo"));
	}

	#[test]
	fn config_skip_repo_returns_false_for_duplicate() {
		let mut config = Config::default();
		config.skip_repo("user/repo");
		assert!(!config.skip_repo("user/repo"));
	}

	#[test]
	fn config_unskip_repo_returns_true_when_was_skipped() {
		let mut config = Config::default();
		config.skip_repo("user/repo");
		assert!(config.unskip_repo("user/repo"));
	}

	#[test]
	fn config_unskip_repo_returns_false_when_not_skipped() {
		let mut config = Config::default();
		assert!(!config.unskip_repo("user/repo"));
	}

	#[test]
	fn config_skip_repo_does_not_affect_other_repos() {
		let mut config = Config::default();
		config.skip_repo("user/repo");
		assert!(!config.is_skipped("user/other"));
	}

	#[test]
	fn config_unskip_repo_clears_skip() {
		let mut config = Config::default();
		config.skip_repo("user/repo");
		config.unskip_repo("user/repo");
		assert!(!config.is_skipped("user/repo"));
	}

	#[test]
	fn config_is_skipped_false_for_unknown() {
		let config = Config::default();
		assert!(!config.is_skipped("user/repo"));
	}

	#[test]
	fn state_mark_synced_stores_pushed_at() {
		let mut state = State::default();
		let t = chrono::Utc::now();
		state.mark_synced("user/repo", Some(t));
		let stored = state.repos["user/repo"].pushed_at;
		assert!(stored.is_some());
	}

	#[test]
	fn state_mark_synced_stores_none_pushed_at() {
		let mut state = State::default();
		state.mark_synced("user/repo", None);
		assert!(state.repos["user/repo"].pushed_at.is_none());
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
		config.pin_repo_with_id("user/repo", None);
		assert!(config.is_pinned("user/repo"));
	}

	#[test]
	fn config_pin_repo_returns_true_for_new_pin() {
		let mut config = Config::default();
		assert!(config.pin_repo_with_id("user/repo", None));
	}

	#[test]
	fn config_pin_repo_returns_false_for_duplicate() {
		let mut config = Config::default();
		config.pin_repo_with_id("user/repo", None);
		assert!(!config.pin_repo_with_id("user/repo", None));
	}

	#[test]
	fn config_unpin_repo_returns_true_when_was_pinned() {
		let mut config = Config::default();
		config.pin_repo_with_id("user/repo", None);
		assert!(config.unpin_repo("user/repo"));
	}

	#[test]
	fn config_unpin_repo_returns_false_when_not_pinned() {
		let mut config = Config::default();
		assert!(!config.unpin_repo("user/repo"));
	}

	#[test]
	fn config_is_pinned_false_for_unknown() {
		let config = Config::default();
		assert!(!config.is_pinned("user/repo"));
	}

	#[test]
	fn pinned_repo_deserializes_legacy_bare_string() {
		let repo: PinnedRepo = toml::Value::String("alice/repo".to_string()).try_into().unwrap();
		assert_eq!(repo.full_name, "alice/repo");
		assert_eq!(repo.id, None);
	}

	#[test]
	fn config_loads_legacy_pinned_string_array() {
		let raw = r#"pinned = ["alice/repo", "bob/other"]"#;
		let config: Config = toml::from_str(raw).expect("legacy pinned string array should still deserialize");
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
		let config: Config = toml::from_str(raw).expect("new pinned table array should deserialize");
		assert!(config.is_pinned("alice/repo"));
		assert_eq!(config.pinned_id("alice/repo"), Some(42));
	}

	#[test]
	fn config_remove_pins_for_user_removes_matching() {
		let mut config = Config::default();
		config.pin_repo_with_id("alice/foo", None);
		config.pin_repo_with_id("alice/bar", None);
		config.pin_repo_with_id("bob/baz", None);
		let removed = config.remove_pins_for_user("alice");
		assert_eq!(removed.len(), 2);
		assert!(!config.is_pinned("alice/foo"));
		assert!(!config.is_pinned("alice/bar"));
		assert!(config.is_pinned("bob/baz"));
	}

	#[test]
	fn config_remove_pins_for_user_case_insensitive() {
		let mut config = Config::default();
		config.pin_repo_with_id("Alice/foo", None);
		let removed = config.remove_pins_for_user("alice");
		assert_eq!(removed.len(), 1);
		assert!(!config.is_pinned("Alice/foo"));
	}

	#[test]
	fn config_remove_pins_for_user_returns_empty_when_none() {
		let mut config = Config::default();
		config.pin_repo_with_id("bob/baz", None);
		let removed = config.remove_pins_for_user("alice");
		assert!(removed.is_empty());
	}

	#[test]
	fn tracked_user_id_defaults_to_none_when_deserializing_legacy_toml() {
		let user: TrackedUser = toml::from_str(r#"name = "alice""#).unwrap();
		assert_eq!(user.id, None);
	}

	#[test]
	fn tracked_user_id_round_trips() {
		let user = TrackedUser { id: Some(42), ..TrackedUser::with_options("alice", false, false) };
		let raw = toml::to_string(&user).unwrap();
		let back: TrackedUser = toml::from_str(&raw).unwrap();
		assert_eq!(back.id, Some(42));
	}

	#[test]
	fn tracked_user_id_omitted_from_toml_when_none() {
		let user = TrackedUser::with_options("alice", false, false);
		let raw = toml::to_string(&user).unwrap();
		assert!(!raw.contains("id"), "got: {raw}");
	}

	#[test]
	fn pin_repo_stores_id() {
		let mut config = Config::default();
		config.pin_repo_with_id("alice/repo", Some(7));
		assert_eq!(config.pinned_id("alice/repo"), Some(7));
	}

	#[test]
	fn pin_repo_without_id_stores_none() {
		let mut config = Config::default();
		config.pin_repo_with_id("alice/repo", None);
		assert_eq!(config.pinned_id("alice/repo"), None);
	}

	#[test]
	fn pin_repo_with_id_returns_false_for_duplicate_full_name() {
		let mut config = Config::default();
		config.pin_repo_with_id("alice/repo", None);
		assert!(!config.pin_repo_with_id("alice/repo", Some(7)));
	}

	#[test]
	fn rename_pin_updates_full_name_and_keeps_id() {
		let mut config = Config::default();
		config.pin_repo_with_id("alice/repo", Some(7));
		assert!(config.rename_pin("alice/repo", "bob/repo"));
		assert!(config.is_pinned("bob/repo"));
		assert!(!config.is_pinned("alice/repo"));
		assert_eq!(config.pinned_id("bob/repo"), Some(7));
	}

	#[test]
	fn rename_pin_returns_false_when_source_missing() {
		let mut config = Config::default();
		assert!(!config.rename_pin("alice/repo", "bob/repo"));
	}
}
