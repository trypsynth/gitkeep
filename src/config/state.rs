use std::{
	collections::{BTreeMap, HashSet},
	fs, mem,
	path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use dirs::home_dir;
use serde::{Deserialize, Serialize};
use toml::{from_str, to_string_pretty};

use crate::utils::write_atomic;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
	#[serde(default)]
	pub repos: BTreeMap<String, RepoState>,
	#[serde(default, skip_serializing)]
	pub skipped: HashSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoState {
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub pushed_at: Option<DateTime<Utc>>,
	/// Stable repo id, used to spot an `owner/name` that was deleted and recreated, which needs a
	/// re-clone rather than a pull.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<u64>,
}

impl State {
	pub fn path() -> Result<PathBuf> {
		let home = home_dir().context("Could not find your home directory")?;
		Ok(home.join(".gitkeep_state.toml"))
	}

	pub fn load() -> Result<Self> {
		Self::load_from(&Self::path()?)
	}

	fn load_from(path: &Path) -> Result<Self> {
		if !path.exists() {
			return Ok(Self::default());
		}
		let raw = fs::read_to_string(path).with_context(|| format!("Could not read state from {}", path.display()))?;
		match from_str(&raw) {
			Ok(state) => Ok(state),
			// The state only saves work, so a sync can carry on without it, but keep the broken file
			// instead of letting the next save overwrite it.
			Err(e) => {
				let backup = path.with_extension("toml.bak");
				fs::rename(path, &backup).with_context(|| {
					format!("State at {} is not valid TOML ({e}), and could not be moved aside", path.display())
				})?;
				eprintln!(
					"Warning: State at {} is not valid TOML, so every repo will be pulled this time. The old file is at {}.",
					path.display(),
					backup.display()
				);
				Ok(Self::default())
			}
		}
	}

	pub fn save(&self) -> Result<()> {
		let path = Self::path()?;
		let raw = to_string_pretty(self).context("Could not serialize state")?;
		write_atomic(&path, &raw).with_context(|| format!("Could not write state to {}", path.display()))
	}

	pub fn mark_synced(&mut self, full_name: &str, pushed_at: Option<DateTime<Utc>>, id: Option<u64>) {
		self.repos.insert(full_name.to_string(), RepoState { pushed_at, id });
	}

	pub fn drain_legacy_skipped(&mut self) -> HashSet<String> {
		mem::take(&mut self.skipped)
	}
}

#[cfg(test)]
mod tests {
	use std::{env, process};

	use super::*;

	#[test]
	fn corrupt_state_is_moved_aside_and_loads_empty() {
		let dir = env::temp_dir().join(format!("gitkeep-state-test-{}", process::id()));
		fs::create_dir_all(&dir).unwrap();
		let path = dir.join("state.toml");
		fs::write(&path, "not [valid toml").unwrap();
		let state = State::load_from(&path).unwrap();
		assert!(state.repos.is_empty());
		assert!(!path.exists());
		assert_eq!(fs::read_to_string(dir.join("state.toml.bak")).unwrap(), "not [valid toml");
		fs::remove_dir_all(&dir).unwrap();
	}

	#[test]
	fn state_with_the_old_last_synced_at_field_still_loads() {
		let state: State = from_str(
			"[repos.\"user/repo\"]\nlast_synced_at = \"2026-01-01T00:00:00Z\"\npushed_at = \"2026-01-01T00:00:00Z\"\nid = 7\n",
		)
		.unwrap();
		assert_eq!(state.repos["user/repo"].id, Some(7));
		assert!(!to_string_pretty(&state).unwrap().contains("last_synced_at"));
	}

	#[test]
	fn state_mark_synced_stores_pushed_at() {
		let mut state = State::default();
		let t = Utc::now();
		state.mark_synced("user/repo", Some(t), Some(1));
		let stored = state.repos["user/repo"].pushed_at;
		assert!(stored.is_some());
	}

	#[test]
	fn state_mark_synced_stores_none_pushed_at() {
		let mut state = State::default();
		state.mark_synced("user/repo", None, Some(1));
		assert!(state.repos["user/repo"].pushed_at.is_none());
	}

	#[test]
	fn state_mark_synced_stores_id() {
		let mut state = State::default();
		state.mark_synced("user/repo", None, Some(42));
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
}
