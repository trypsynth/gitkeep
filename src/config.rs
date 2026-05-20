use std::{
	collections::{HashMap, HashSet},
	fs,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedUser {
	pub name: String,
	#[serde(default, skip_serializing_if = "is_false")]
	pub forks: bool,
	#[serde(default, skip_serializing_if = "is_false")]
	pub frozen: bool,
}

impl TrackedUser {
	pub fn with_options(name: impl Into<String>, forks: bool, frozen: bool) -> Self {
		Self { name: name.into(), forks, frozen }
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
			let canonical_changed = if entry.name != user {
				entry.name = user.to_string();
				true
			} else {
				false
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

	pub fn skip_repo(&mut self, full_name: &str) {
		self.skipped.insert(full_name.to_string());
	}

	pub fn unskip_repo(&mut self, full_name: &str) {
		self.skipped.remove(full_name);
	}

	pub fn is_skipped(&self, full_name: &str) -> bool {
		self.skipped.contains(full_name)
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

	pub fn mark_synced(&mut self, full_name: &str) {
		self.repos.insert(full_name.to_string(), RepoState { last_synced_at: Utc::now() });
	}

	pub fn drain_legacy_skipped(&mut self) -> HashSet<String> {
		std::mem::take(&mut self.skipped)
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
