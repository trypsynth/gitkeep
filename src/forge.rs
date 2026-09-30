//! The code forges gitkeep can archive from. GitHub is the default; any other host is a
//! self-hosted forge whose kind is recorded in the config's `[hosts]` table.

mod github;
mod gitlab;
mod http;

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use self::http::Http;
pub use self::{github::GitHub, gitlab::GitLab};

/// The kind of software a self-hosted forge runs, which decides the API gitkeep speaks to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForgeKind {
	GitLab,
}

impl ForgeKind {
	pub const fn name(self) -> &'static str {
		match self {
			Self::GitLab => "GitLab",
		}
	}
}

/// A connected forge. Every forge answers the same questions, so sync, add, and remove never need
/// to know which one they're talking to.
pub enum Forge {
	GitHub(GitHub),
	GitLab(GitLab),
}

/// What an `add` target turned out to be.
pub enum Resolved {
	/// A whole account (user, org, or group), by its canonical name on the forge.
	Account(String),
	Repo(RemoteRepo),
}

/// An account's repos, along with its current name and stable id (when the forge has one), so
/// renames can be followed.
pub struct AccountRepos {
	pub name: String,
	pub id: Option<u64>,
	pub repos: Vec<RemoteRepo>,
}

/// Forge-neutral details for one remote repository. `full_name` is its key in the config and
/// state files (`owner/repo` on GitHub, `host/namespace/project` elsewhere); `rel_dir` is its
/// '/'-separated path under the archive root.
pub struct RemoteRepo {
	pub full_name: String,
	pub rel_dir: String,
	pub id: u64,
	/// When the repo was last pushed to, if the forge reports it reliably. `None` makes every sync
	/// pull the repo rather than skipping it as unchanged.
	pub pushed_at: Option<DateTime<Utc>>,
	pub fork: bool,
	pub clone_url: Option<String>,
	pub ssh_url: Option<String>,
}

impl RemoteRepo {
	pub fn clone_url(&self, use_ssh: bool) -> Option<String> {
		if use_ssh { self.ssh_url.clone() } else { self.clone_url.clone() }
	}
}

impl Forge {
	/// Resolves an `add` target path (the part after the host) to an account or a single repo.
	pub async fn resolve(&self, path: &str) -> Result<Resolved> {
		match self {
			Self::GitHub(f) => f.resolve(path).await,
			Self::GitLab(f) => f.resolve(path).await,
		}
	}

	/// Lists a tracked account's repos. `id` is the account's stored id, used to follow renames.
	pub async fn account(&self, name: &str, id: Option<u64>) -> Result<AccountRepos> {
		match self {
			Self::GitHub(f) => f.account(name, id).await,
			Self::GitLab(f) => f.account(name).await,
		}
	}

	/// Fetches one repo by its path on the forge. `id` is its stored id, used to follow renames.
	pub async fn repo(&self, path: &str, id: Option<u64>) -> Result<Option<RemoteRepo>> {
		match self {
			Self::GitHub(f) => f.repo(path, id).await,
			Self::GitLab(f) => f.repo(path).await,
		}
	}
}

/// Works out which kind of forge `host` runs, for the first time something is added from it.
pub async fn detect(host: &str) -> Result<ForgeKind> {
	let http = Http::new(host, None)?;
	if GitLab::detect(&http).await? {
		return Ok(ForgeKind::GitLab);
	}
	bail!("Couldn't recognize {host} as a supported forge (GitLab).")
}

/// A target given on the command line: the forge host it lives on (`None` for GitHub) and its path
/// there, e.g. `rust-lang/mdBook` or `some-group/sub/project`.
#[derive(Debug, PartialEq, Eq)]
pub struct Target {
	pub host: Option<String>,
	pub path: String,
}

impl Target {
	/// Accepts a URL (`https://host/path`), a host-qualified name (`host/path`), or a plain GitHub
	/// name (`owner` or `owner/repo`). github.com URLs become plain GitHub names.
	pub fn parse(arg: &str) -> Self {
		if let Some((host, path)) = parse_url(arg) {
			let host = (host != "github.com").then_some(host);
			return Self { host, path };
		}
		match split_host(arg) {
			Some((host, path)) => Self { host: Some(host.to_lowercase()), path: path.to_string() },
			None => Self { host: None, path: arg.to_string() },
		}
	}

	/// The target's key in the config and in output: `path` on GitHub, `host/path` elsewhere.
	pub fn key(&self) -> String {
		self.host.as_ref().map_or_else(|| self.path.clone(), |h| format!("{h}/{}", self.path))
	}
}

/// Parses an `https://host/path` (or `http://`) argument into `(host, path)`, trimming trailing
/// slashes and a `.git` suffix. Returns `None` for non-URL arguments.
fn parse_url(arg: &str) -> Option<(String, String)> {
	let rest = arg.strip_prefix("https://").or_else(|| arg.strip_prefix("http://"))?;
	let (host, path) = rest.split_once('/')?;
	let path = path.trim_end_matches('/');
	let path = path.strip_suffix(".git").unwrap_or(path).trim_end_matches('/');
	if host.is_empty() || path.is_empty() {
		return None;
	}
	Some((host.to_lowercase(), path.to_string()))
}

/// Where a config key (an account or a repo, e.g. `alice`, `alice/repo`, or
/// `gitlab.com/grp/proj`) lives under the archive root. Keys double as relative paths.
pub fn archive_path(root: &Path, key: &str) -> PathBuf {
	let mut path = root.to_path_buf();
	path.extend(key.split('/'));
	path
}

/// Splits a host-qualified key (`host/namespace/project`) into host and path. GitHub keys
/// (`owner/repo`) return `None`: GitHub usernames can't contain dots while hosts always do, so the
/// first segment tells them apart.
pub fn split_host(key: &str) -> Option<(&str, &str)> {
	let (first, rest) = key.split_once('/')?;
	(first.contains('.') && !rest.is_empty()).then_some((first, rest))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn target(host: Option<&str>, path: &str) -> Target {
		Target { host: host.map(ToString::to_string), path: path.to_string() }
	}

	#[test]
	fn parse_url_extracts_host_and_path() {
		assert_eq!(
			parse_url("https://gitlab.example.com/some-group/project"),
			Some(("gitlab.example.com".to_string(), "some-group/project".to_string()))
		);
	}

	#[test]
	fn parse_url_trims_git_suffix_and_slash() {
		assert_eq!(parse_url("https://gitlab.example.com/group/repo.git/").unwrap().1, "group/repo");
	}

	#[test]
	fn parse_url_lowercases_host() {
		assert_eq!(parse_url("https://GitLab.Example.COM/group").unwrap().0, "gitlab.example.com");
	}

	#[test]
	fn parse_url_rejects_plain_names_and_bare_hosts() {
		assert!(parse_url("rust-lang").is_none());
		assert!(parse_url("rust-lang/mdBook").is_none());
		assert!(parse_url("https://gitlab.example.com").is_none());
		assert!(parse_url("https://gitlab.example.com/").is_none());
	}

	#[test]
	fn split_host_recognizes_host_qualified_keys() {
		assert_eq!(
			split_host("gitlab.example.com/some-group/project"),
			Some(("gitlab.example.com", "some-group/project"))
		);
	}

	#[test]
	fn split_host_rejects_github_names() {
		assert!(split_host("rust-lang/mdBook").is_none());
		assert!(split_host("rust-lang").is_none());
	}

	#[test]
	fn target_parse_handles_every_form() {
		assert_eq!(Target::parse("rust-lang"), target(None, "rust-lang"));
		assert_eq!(Target::parse("rust-lang/mdBook"), target(None, "rust-lang/mdBook"));
		assert_eq!(Target::parse("https://github.com/rust-lang/mdBook"), target(None, "rust-lang/mdBook"));
		assert_eq!(Target::parse("https://gitlab.com/grp/proj"), target(Some("gitlab.com"), "grp/proj"));
		assert_eq!(Target::parse("GitLab.com/grp/proj"), target(Some("gitlab.com"), "grp/proj"));
	}

	#[test]
	fn target_key_prefixes_host() {
		assert_eq!(target(None, "alice/repo").key(), "alice/repo");
		assert_eq!(target(Some("gitlab.com"), "grp/proj").key(), "gitlab.com/grp/proj");
	}
}
