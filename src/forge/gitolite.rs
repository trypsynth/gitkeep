use std::process::Stdio;

use anyhow::{Context, Result, bail};
use tokio::{process::Command, sync::OnceCell};

use super::{AccountRepos, RemoteRepo, Resolved};

/// A gitolite server. It has no web API, so everything goes through `ssh <ssh> info`, which lists
/// the repos the user's key can read. Repos have no ids or push times, so every sync pulls them.
pub struct Gitolite {
	host: String,
	/// What to give `ssh` and `git clone`: the SSH alias the host was added through, or the host.
	ssh: String,
	repos: OnceCell<Vec<String>>,
}

impl Gitolite {
	pub fn new(host: &str, ssh: &str) -> Self {
		Self { host: host.to_string(), ssh: ssh.to_string(), repos: OnceCell::new() }
	}

	/// Whether `ssh` reaches a gitolite server, which greets `info` with its version.
	pub async fn detect(ssh: &str) -> bool {
		info(ssh).await.is_ok_and(|out| out.lines().next().is_some_and(|l| l.contains("gitolite")))
	}

	/// Resolves an `add` target: a readable repo, or a path with repos under it (all of them for an
	/// empty path) as an account.
	pub async fn resolve(&self, path: &str) -> Result<Resolved> {
		let repos = self.repos().await?;
		if repos.iter().any(|r| r == path) {
			return Ok(Resolved::Repo(self.remote_repo(path)));
		}
		if repos.iter().any(|r| is_under(r, path)) {
			return Ok(Resolved::Account(path.to_string()));
		}
		bail!("'{path}' isn't a repo you can read on {}, or a folder of them.", self.host)
	}

	pub async fn account(&self, name: &str) -> Result<AccountRepos> {
		let repos = self.repos().await?.iter().filter(|r| is_under(r, name)).map(|r| self.remote_repo(r)).collect();
		Ok(AccountRepos { name: name.to_string(), id: None, repos })
	}

	pub async fn repo(&self, path: &str) -> Result<Option<RemoteRepo>> {
		Ok(self.repos().await?.iter().any(|r| r == path).then(|| self.remote_repo(path)))
	}

	async fn repos(&self) -> Result<&Vec<String>> {
		self.repos
			.get_or_try_init(|| async {
				let out = info(&self.ssh).await.with_context(|| format!("Could not list repos on {}", self.host))?;
				Ok(parse_info(&out))
			})
			.await
	}

	fn remote_repo(&self, name: &str) -> RemoteRepo {
		let full_name = format!("{}/{name}", self.host);
		let url = format!("{}:{name}", self.ssh);
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

async fn info(ssh: &str) -> Result<String> {
	let output = Command::new("ssh")
		.args([ssh, "info"])
		.stdin(Stdio::null())
		.output()
		.await
		.context("Could not run 'ssh'. Is it installed and on your PATH?")?;
	if !output.status.success() {
		bail!("'ssh {ssh} info' failed: {}", String::from_utf8_lossy(&output.stderr).trim());
	}
	Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The readable repos in `info` output: lines of permissions, a tab, the name, and optionally a tab
/// and description. Wildcard repo patterns are listed too, but they aren't repos.
fn parse_info(out: &str) -> Vec<String> {
	out.lines()
		.filter_map(|line| {
			let mut fields = line.split('\t');
			let (perms, name) = (fields.next()?, fields.next()?.trim());
			let literal = name.chars().all(|c| c.is_ascii_alphanumeric() || "._-/+@".contains(c));
			(perms.contains('R') && literal && !name.is_empty()).then(|| name.to_string())
		})
		.collect()
}

/// Whether the repo `name` is inside the folder `path`, where an empty path is the whole server.
fn is_under(name: &str, path: &str) -> bool {
	path.is_empty() || name.strip_prefix(path).is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parse_info_keeps_readable_repos_and_skips_the_greeting_and_patterns() {
		let out = "hello quin, this is git@storage running gitolite3 v3.6.14 on git 2.55.0\n\n \
		           R W\tsrc\tUnnamed repository\n R  \ttools/txt\n C  R W\tusers/CREATOR/..*\n    W\twriteonly\n";
		assert_eq!(parse_info(out), ["src", "tools/txt"]);
	}

	#[test]
	fn is_under_matches_whole_folders_only() {
		assert!(is_under("src", ""));
		assert!(is_under("tools/txt", "tools"));
		assert!(!is_under("toolsx/txt", "tools"));
		assert!(!is_under("tools", "tools"));
	}

	#[test]
	fn remote_repo_clones_through_the_alias_under_the_host() {
		let repo = Gitolite::new("home.example.com", "chats").remote_repo("src");
		assert_eq!(repo.full_name, "home.example.com/src");
		assert_eq!(repo.rel_dir, "home.example.com/src");
		assert_eq!(repo.clone_url.as_deref(), Some("chats:src"));
		assert_eq!(repo.ssh_url.as_deref(), Some("chats:src"));
		assert!(repo.id.is_none() && repo.pushed_at.is_none());
	}
}
