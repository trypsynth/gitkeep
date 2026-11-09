use std::process::Stdio;

use anyhow::{Context, Result, bail};
use tokio::process::Command;

use super::{AccountRepos, RemoteRepo, Resolved};

/// A host that serves git but no API gitkeep knows, such as cgit or a bare SSH server. Without a
/// way to list an account's repos, only single repos can be tracked, and every sync pulls them.
pub struct PlainGit {
	host: String,
	/// The SSH alias repos are cloned through, or `None` to clone over HTTPS.
	ssh: Option<String>,
}

impl PlainGit {
	pub fn new(host: &str, ssh: Option<&str>) -> Self {
		Self { host: host.to_string(), ssh: ssh.map(ToString::to_string) }
	}

	/// Resolves an `add` target, which can only be a repo: `git ls-remote` has to reach it.
	pub async fn resolve(&self, path: &str) -> Result<Resolved> {
		if path.is_empty() {
			bail!("{} has no way to list its repos, so add them one at a time.", self.host);
		}
		let url = self.url(path);
		let output = Command::new("git")
			.args(["ls-remote", "--", &url, "HEAD"])
			.env("GIT_TERMINAL_PROMPT", "0")
			.stdin(Stdio::null())
			.output()
			.await
			.context("Could not run 'git'. Is it installed and on your PATH?")?;
		if !output.status.success() {
			let stderr = String::from_utf8_lossy(&output.stderr);
			let reason = stderr.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("no reason given").trim();
			bail!(
				"Could not reach {url} as a git repo ({reason}). {} has no way to list an account's repos, so only single repos can be added from it.",
				self.host
			);
		}
		Ok(Resolved::Repo(self.remote_repo(path)))
	}

	pub fn account(&self, name: &str) -> Result<AccountRepos> {
		bail!("{} has no way to list {name}'s repos, so it can't be tracked as a whole account.", self.host)
	}

	/// There's nothing to look up, so a repo that's gone only shows when its pull fails.
	pub fn repo(&self, path: &str) -> RemoteRepo {
		self.remote_repo(path)
	}

	fn url(&self, path: &str) -> String {
		self.ssh.as_ref().map_or_else(|| format!("https://{}/{path}", self.host), |ssh| format!("{ssh}:{path}"))
	}

	fn remote_repo(&self, path: &str) -> RemoteRepo {
		let full_name = format!("{}/{path}", self.host);
		let url = self.url(path);
		RemoteRepo {
			rel_dir: full_name.clone(),
			full_name,
			id: None,
			pushed_at: None,
			fork: false,
			clone_url: Some(url.clone()),
			ssh_url: Some(url),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn clones_over_https_or_through_the_alias() {
		let https = PlainGit::new("git.example.com", None).repo("grace/dots");
		assert_eq!(https.full_name, "git.example.com/grace/dots");
		assert_eq!(https.clone_url.as_deref(), Some("https://git.example.com/grace/dots"));
		assert_eq!(https.ssh_url, https.clone_url, "use_ssh can't pick a URL the host doesn't have");
		let ssh = PlainGit::new("box.example.com", Some("box")).repo("src");
		assert_eq!(ssh.clone_url.as_deref(), Some("box:src"));
	}

	#[test]
	fn whole_accounts_are_refused() {
		assert!(PlainGit::new("git.example.com", None).account("grace").is_err());
	}
}
