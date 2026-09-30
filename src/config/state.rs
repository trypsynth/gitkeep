use std::{
	collections::{HashMap, HashSet},
	fs, mem,
	path::PathBuf,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use dirs::home_dir;
use serde::{Deserialize, Serialize};
use toml::{from_str, to_string_pretty};

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
	use super::*;

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
}
