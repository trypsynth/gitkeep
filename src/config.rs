use std::{collections::HashMap, fs, path::PathBuf};

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedUser {
	pub name: String,
	#[serde(default, skip_serializing_if = "is_false")]
	pub forks: bool,
}

impl TrackedUser {
	pub fn new(name: impl Into<String>) -> Self {
		Self { name: name.into(), forks: false }
	}

	pub fn with_forks(name: impl Into<String>) -> Self {
		Self { name: name.into(), forks: true }
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

	pub fn is_tracked(&self, name: &str) -> bool {
		self.track.iter().any(|u| u.name == name)
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

	pub fn add_user(&mut self, user: &str, forks: bool) -> bool {
		if let Some(entry) = self.track.iter_mut().find(|u| u.name == user) {
			if forks && !entry.forks {
				entry.forks = true;
				println!("Forks enabled for {user}.");
				true
			} else {
				println!("Already tracking {user}.");
				false
			}
		} else {
			let entry = if forks { TrackedUser::with_forks(user) } else { TrackedUser::new(user) };
			println!("Now tracking {}{}", user, if forks { " (forks included)" } else { "" });
			self.track.push(entry);
			true
		}
	}

	pub fn remove_user(&mut self, user: &str) -> bool {
		let before = self.track.len();
		self.track.retain(|u| u.name != user);
		if self.track.len() < before {
			println!("Stopped tracking {user}.");
			true
		} else {
			println!("Not tracking {user}.");
			false
		}
	}
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
	#[serde(default)]
	pub repos: HashMap<String, RepoState>,
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
}
