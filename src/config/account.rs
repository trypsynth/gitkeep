use serde::{Deserialize, Deserializer, Serialize};

use crate::forge::{ForgeKind, split_host};

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_false(v: &bool) -> bool {
	!*v
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
	/// Stable account id on its forge, used to follow renames.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<u64>,
	/// Overrides the global `submodules` default for this account. `None` inherits it.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub submodules: Option<bool>,
	/// Forge host this account lives on, a key of `Config::hosts`. `None` means GitHub.
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

	/// Whether this account's sync already includes `full_name`, including projects in GitLab
	/// subgroups.
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
	/// Stable repo id on its forge, used to follow renames.
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

#[cfg(test)]
mod tests {
	use toml::{Value, from_str, to_string};

	use super::*;

	#[test]
	fn pinned_repo_deserializes_legacy_bare_string() {
		let repo: PinnedRepo = Value::String("alice/repo".to_string()).try_into().unwrap();
		assert_eq!(repo.full_name, "alice/repo");
		assert_eq!(repo.id, None);
	}

	#[test]
	fn tracked_account_submodules_defaults_to_none_for_legacy_toml() {
		let user: TrackedAccount = from_str(r#"name = "alice""#).unwrap();
		assert_eq!(user.submodules, None);
	}

	#[test]
	fn tracked_account_submodules_round_trips() {
		let user = TrackedAccount { submodules: Some(true), ..TrackedAccount::with_options("alice", false, false) };
		let raw = to_string(&user).unwrap();
		let back: TrackedAccount = from_str(&raw).unwrap();
		assert_eq!(back.submodules, Some(true));
	}

	#[test]
	fn tracked_account_id_defaults_to_none_when_deserializing_legacy_toml() {
		let user: TrackedAccount = from_str(r#"name = "alice""#).unwrap();
		assert_eq!(user.id, None);
	}

	#[test]
	fn tracked_account_id_round_trips() {
		let user = TrackedAccount { id: Some(42), ..TrackedAccount::with_options("alice", false, false) };
		let raw = to_string(&user).unwrap();
		let back: TrackedAccount = from_str(&raw).unwrap();
		assert_eq!(back.id, Some(42));
	}

	#[test]
	fn tracked_account_id_omitted_from_toml_when_none() {
		let user = TrackedAccount::with_options("alice", false, false);
		let raw = to_string(&user).unwrap();
		assert!(!raw.contains("id"), "got: {raw}");
	}

	#[test]
	fn tracked_account_host_defaults_to_none_for_legacy_toml() {
		let user: TrackedAccount = from_str(r#"name = "alice""#).unwrap();
		assert_eq!(user.host, None);
	}

	#[test]
	fn tracked_account_host_round_trips() {
		let user = TrackedAccount {
			host: Some("gitlab.example.com".to_string()),
			..TrackedAccount::with_options("grp", false, false)
		};
		let raw = to_string(&user).unwrap();
		let back: TrackedAccount = from_str(&raw).unwrap();
		assert_eq!(back.host.as_deref(), Some("gitlab.example.com"));
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
}
