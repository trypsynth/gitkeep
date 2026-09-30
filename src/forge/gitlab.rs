use anyhow::{Result, bail};
use serde::Deserialize;

use super::{AccountRepos, RemoteRepo, Resolved, http::Http};

/// A project as returned by the GitLab REST API v4, reduced to the fields gitkeep needs.
#[derive(Debug, Clone, Deserialize)]
struct Project {
	id: u64,
	path_with_namespace: String,
	http_url_to_repo: Option<String>,
	ssh_url_to_repo: Option<String>,
	#[serde(default, rename = "forked_from_project")]
	forked_from: Option<ForkParent>,
}

/// Marker for `forked_from_project`; only its presence matters for fork detection.
#[derive(Debug, Clone, Deserialize)]
struct ForkParent {}

#[derive(Deserialize)]
struct GroupInfo {
	full_path: String,
}

#[derive(Deserialize)]
struct UserInfo {
	username: String,
}

/// A client for one GitLab instance, speaking the REST API v4.
pub struct GitLab {
	http: Http,
}

impl GitLab {
	pub fn new(host: &str, token: Option<&str>) -> Result<Self> {
		Ok(Self { http: Http::new(host, token)? })
	}

	pub fn host(&self) -> &str {
		self.http.host()
	}

	/// Whether `http` points at a GitLab instance. `/api/v4/version` answers 200 when signed in and
	/// 401 otherwise; anything else (usually a 404 page) means it isn't GitLab.
	pub async fn detect(http: &Http) -> Result<bool> {
		Ok(matches!(http.status("/api/v4/version").await?, 200 | 401))
	}

	/// Resolves an `add` target path to a whole namespace (group or user) to track or a single
	/// project to pin. Paths with a slash are checked as a project first, since a project path can
	/// never be a bare top-level name.
	pub async fn resolve(&self, path: &str) -> Result<Resolved> {
		if path.contains('/')
			&& let Some(p) = self.project(path).await?
		{
			return Ok(Resolved::Repo(self.remote_repo(&p)));
		}
		if let Some(g) = self.group(path).await? {
			return Ok(Resolved::Account(g.full_path));
		}
		if !path.contains('/') {
			let users: Vec<UserInfo> =
				self.http.get(&format!("/api/v4/users?username={path}")).await?.unwrap_or_default();
			if let Some(u) = users.into_iter().next() {
				return Ok(Resolved::Account(u.username));
			}
		}
		bail!("Could not find '{path}' on {} (checked project, group, and user)", self.host())
	}

	/// Lists every project under a namespace. Group listings include subgroups, so a tracked
	/// top-level group archives its whole tree. GitLab namespaces aren't followed across renames.
	pub async fn account(&self, name: &str) -> Result<AccountRepos> {
		let projects = if self.group(name).await?.is_some() {
			self.paged(&format!("/api/v4/groups/{}/projects?include_subgroups=true", encode_path(name))).await?
		} else {
			self.paged(&format!("/api/v4/users/{name}/projects")).await?
		};
		let repos = projects.iter().map(|p| self.remote_repo(p)).collect();
		Ok(AccountRepos { name: name.to_string(), id: None, repos })
	}

	/// Fetches one project by its `namespace/project` path, or `None` if it doesn't exist.
	pub async fn repo(&self, path: &str) -> Result<Option<RemoteRepo>> {
		Ok(self.project(path).await?.map(|p| self.remote_repo(&p)))
	}

	async fn project(&self, path: &str) -> Result<Option<Project>> {
		self.http.get(&format!("/api/v4/projects/{}", encode_path(path))).await
	}

	async fn group(&self, path: &str) -> Result<Option<GroupInfo>> {
		self.http.get(&format!("/api/v4/groups/{}?with_projects=false", encode_path(path))).await
	}

	/// Follows page-number pagination until a short page. Ordered by id so pages stay stable if
	/// projects are created mid-listing.
	async fn paged(&self, base: &str) -> Result<Vec<Project>> {
		let sep = if base.contains('?') { '&' } else { '?' };
		let mut all = Vec::new();
		for page in 1u32.. {
			let batch: Vec<Project> = self
				.http
				.get(&format!("{base}{sep}order_by=id&sort=asc&per_page=100&page={page}"))
				.await?
				.unwrap_or_default();
			let done = batch.len() < 100;
			all.extend(batch);
			if done {
				break;
			}
		}
		Ok(all)
	}

	fn remote_repo(&self, project: &Project) -> RemoteRepo {
		let full_name = format!("{}/{}", self.host(), project.path_with_namespace);
		RemoteRepo {
			rel_dir: full_name.clone(),
			full_name,
			id: project.id,
			// GitLab's `last_activity_at` updates at most once an hour, so using it to skip pulls could
			// miss recent pushes. Leaving this unset makes every sync pull.
			pushed_at: None,
			fork: project.forked_from.is_some(),
			clone_url: project.http_url_to_repo.clone(),
			ssh_url: project.ssh_url_to_repo.clone(),
		}
	}
}

/// GitLab addresses projects by URL-encoded full path; group and project paths only contain
/// letters, digits, `_`, `-`, and `.`, so only the separators need encoding.
fn encode_path(path: &str) -> String {
	path.replace('/', "%2F")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn encode_path_escapes_separators_only() {
		assert_eq!(encode_path("group/sub/project"), "group%2Fsub%2Fproject");
		assert_eq!(encode_path("plain"), "plain");
	}

	#[tokio::test]
	async fn remote_repo_prefixes_host_and_always_pulls() {
		let gitlab = GitLab::new("gitlab.example.com", None).unwrap();
		let project = Project {
			id: 7,
			path_with_namespace: "grp/proj".to_string(),
			http_url_to_repo: Some("https://gitlab.example.com/grp/proj.git".to_string()),
			ssh_url_to_repo: Some("git@gitlab.example.com:grp/proj.git".to_string()),
			forked_from: None,
		};
		let repo = gitlab.remote_repo(&project);
		assert_eq!(repo.full_name, "gitlab.example.com/grp/proj");
		assert_eq!(repo.rel_dir, "gitlab.example.com/grp/proj");
		assert!(!repo.fork);
		assert!(repo.pushed_at.is_none(), "GitLab repos must always be pulled");
		assert_eq!(repo.clone_url(false).unwrap(), "https://gitlab.example.com/grp/proj.git");
		assert_eq!(repo.clone_url(true).unwrap(), "git@gitlab.example.com:grp/proj.git");
	}
}
