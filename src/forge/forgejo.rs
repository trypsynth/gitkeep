use anyhow::{Result, bail};
use serde::Deserialize;

use super::{AccountRepos, RemoteRepo, Resolved, http::Http};

/// A repository as returned by the Forgejo (and Gitea) REST API v1, reduced to what gitkeep needs.
#[derive(Deserialize)]
struct Repo {
	id: u64,
	full_name: String,
	#[serde(default)]
	fork: bool,
	clone_url: Option<String>,
	ssh_url: Option<String>,
}

#[derive(Deserialize)]
struct User {
	id: u64,
	login: String,
}

#[derive(Deserialize)]
struct Org {
	name: String,
}

#[derive(Deserialize)]
struct Version {
	#[serde(rename = "version")]
	_version: String,
}

/// Page size requested when listing repos. Instances can cap it lower, so listing stops at an
/// empty page rather than a short one.
const PAGE_SIZE: usize = 50;

/// A client for one Forgejo instance, speaking the REST API v1 it shares with Gitea.
pub struct Forgejo {
	http: Http,
}

impl Forgejo {
	pub fn new(host: &str, token: Option<&str>) -> Result<Self> {
		Ok(Self { http: Http::new(host, token)? })
	}

	pub fn host(&self) -> &str {
		self.http.host()
	}

	/// Whether `http` points at a Forgejo (or Gitea) instance, which answers `/api/v1/version`.
	pub async fn detect(http: &Http) -> bool {
		matches!(http.get::<Version>("/api/v1/version").await, Ok(Some(_)))
	}

	/// Resolves an `add` target: a bare name is a user or org, `owner/repo` a single repo.
	pub async fn resolve(&self, path: &str) -> Result<Resolved> {
		match path.split_once('/') {
			None => {
				if let Some(org) = self.org(path).await? {
					return Ok(Resolved::Account(org.name));
				}
				match self.user(path).await? {
					Some(user) => Ok(Resolved::Account(user.login)),
					None => bail!("Could not find '{path}' on {} (checked user and org)", self.host()),
				}
			}
			Some((owner, name)) if !owner.is_empty() && !name.is_empty() && !name.contains('/') => {
				match self.repo(path, None).await? {
					Some(repo) => Ok(Resolved::Repo(repo)),
					None => bail!("'{path}' does not exist on {}.", self.host()),
				}
			}
			_ => bail!("'{path}' is not in owner/repo format"),
		}
	}

	/// Lists every repo of a user or org. Forgejo redirects renamed accounts, so the returned name is
	/// always the current one.
	pub async fn account(&self, name: &str) -> Result<AccountRepos> {
		let (name, id, base) = if let Some(org) = self.org(name).await? {
			let base = format!("/api/v1/orgs/{}/repos", org.name);
			(org.name, None, base)
		} else if let Some(user) = self.user(name).await? {
			let base = format!("/api/v1/users/{}/repos", user.login);
			(user.login, Some(user.id), base)
		} else {
			bail!("Could not find '{name}' on {}", self.host());
		};
		let mut repos = Vec::new();
		for page in 1u32.. {
			let batch: Vec<Repo> =
				self.http.get(&format!("{base}?limit={PAGE_SIZE}&page={page}")).await?.unwrap_or_default();
			if batch.is_empty() {
				break;
			}
			repos.extend(batch.iter().map(|r| self.remote_repo(r)));
		}
		Ok(AccountRepos { name, id, repos })
	}

	/// Fetches one repo by `owner/repo`, which Forgejo resolves across renames. The stored `id` is a
	/// fallback, though some instances only allow looking up by id with a token.
	pub async fn repo(&self, path: &str, id: Option<u64>) -> Result<Option<RemoteRepo>> {
		let mut repo: Option<Repo> = self.http.get(&format!("/api/v1/repos/{path}")).await?;
		if repo.is_none()
			&& let Some(id) = id
		{
			repo = self.http.get(&format!("/api/v1/repositories/{id}")).await.ok().flatten();
		}
		Ok(repo.map(|r| self.remote_repo(&r)))
	}

	async fn org(&self, name: &str) -> Result<Option<Org>> {
		self.http.get(&format!("/api/v1/orgs/{name}")).await
	}

	async fn user(&self, name: &str) -> Result<Option<User>> {
		self.http.get(&format!("/api/v1/users/{name}")).await
	}

	fn remote_repo(&self, repo: &Repo) -> RemoteRepo {
		let full_name = format!("{}/{}", self.host(), repo.full_name);
		RemoteRepo {
			rel_dir: full_name.clone(),
			full_name,
			id: repo.id,
			// The API has no push timestamp (`updated_at` also moves for other changes, and isn't
			// guaranteed to move on every push), so every sync pulls.
			pushed_at: None,
			fork: repo.fork,
			clone_url: repo.clone_url.clone(),
			ssh_url: repo.ssh_url.clone(),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn remote_repo_prefixes_host_and_always_pulls() {
		let forgejo = Forgejo::new("codeberg.org", None).unwrap();
		let repo = Repo {
			id: 7,
			full_name: "forgejo/forgejo".to_string(),
			fork: true,
			clone_url: Some("https://codeberg.org/forgejo/forgejo.git".to_string()),
			ssh_url: Some("ssh://git@codeberg.org/forgejo/forgejo.git".to_string()),
		};
		let remote = forgejo.remote_repo(&repo);
		assert_eq!(remote.full_name, "codeberg.org/forgejo/forgejo");
		assert_eq!(remote.rel_dir, "codeberg.org/forgejo/forgejo");
		assert!(remote.fork);
		assert!(remote.pushed_at.is_none(), "Forgejo repos must always be pulled");
		assert_eq!(remote.clone_url(true).unwrap(), "ssh://git@codeberg.org/forgejo/forgejo.git");
	}
}
