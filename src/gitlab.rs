use anyhow::{Context, Result, anyhow, bail};
use octocrab::{Octocrab, OctocrabBuilder};
use serde::{Deserialize, de::DeserializeOwned};

/// A project as returned by the GitLab REST API v4, reduced to the fields gitkeep needs.
#[derive(Debug, Clone, Deserialize)]
pub struct Project {
	pub id: u64,
	pub path_with_namespace: String,
	pub http_url_to_repo: Option<String>,
	pub ssh_url_to_repo: Option<String>,
	#[serde(default)]
	#[serde(rename = "forked_from_project")]
	pub forked_from: Option<ForkParent>,
}

/// Marker for `forked_from_project`; only its presence matters for fork detection.
#[derive(Debug, Clone, Deserialize)]
pub struct ForkParent {}

#[derive(Deserialize)]
struct GroupInfo {
	full_path: String,
}

#[derive(Deserialize)]
struct UserInfo {
	username: String,
}

pub enum Target {
	/// A group or user namespace whose projects should all be tracked.
	Namespace(String),
	Project(Box<Project>),
}

/// A thin client for a single GitLab instance. Reuses octocrab as a generic JSON HTTP
/// client (pointed at the instance's base URI) so no extra HTTP dependency is needed;
/// GitLab ignores the GitHub-flavored Accept header and accepts personal access tokens
/// via the same Bearer authorization scheme.
pub struct GitLabClient {
	host: String,
	client: Octocrab,
}

impl GitLabClient {
	pub fn new(host: &str, token: Option<String>) -> Result<Self> {
		let builder = OctocrabBuilder::default()
			.base_uri(format!("https://{host}"))
			.with_context(|| format!("'{host}' is not a valid GitLab host"))?;
		let client = match token {
			Some(t) => builder.personal_token(t).build(),
			None => builder.build(),
		}
		.with_context(|| format!("Could not create client for {host}"))?;
		Ok(Self { host: host.to_string(), client })
	}

	async fn get<T: DeserializeOwned>(&self, path: String) -> Result<T> {
		self.client.get(path, None::<&()>).await.map_err(|e| self.request_error(&e))
	}

	/// Like `get`, but a 404 is `Ok(None)` rather than an error, so callers can tell "doesn't
	/// exist" apart from network or permission failures.
	async fn get_optional<T: DeserializeOwned>(&self, path: String) -> Result<Option<T>> {
		match self.client.get(path, None::<&()>).await {
			Ok(v) => Ok(Some(v)),
			Err(e) if status_of(&e) == Some(404) => Ok(None),
			Err(e) => Err(self.request_error(&e)),
		}
	}

	fn request_error(&self, e: &octocrab::Error) -> anyhow::Error {
		let host = &self.host;
		match status_of(e) {
			Some(401 | 403) => anyhow!(
				"{host} denied the request ({}). For private projects, add a token under [gitlab_tokens] in the config.",
				describe(e)
			),
			_ => anyhow!("Request to {host} failed: {}", describe(e)),
		}
	}

	/// Resolves an `add` target path to either a whole namespace (group or user) to track
	/// or a single project to pin. Paths with a slash are checked as a project first,
	/// since a project path can never be a bare top-level name.
	pub async fn resolve_target(&self, path: &str) -> Result<Target> {
		if path.contains('/')
			&& let Some(p) = self.get_optional::<Project>(format!("/api/v4/projects/{}", encode_path(path))).await?
		{
			return Ok(Target::Project(Box::new(p)));
		}
		if let Some(g) =
			self.get_optional::<GroupInfo>(format!("/api/v4/groups/{}?with_projects=false", encode_path(path))).await?
		{
			return Ok(Target::Namespace(g.full_path));
		}
		if !path.contains('/') {
			let users: Vec<UserInfo> = self.get(format!("/api/v4/users?username={path}")).await?;
			if let Some(u) = users.into_iter().next() {
				return Ok(Target::Namespace(u.username));
			}
		}
		bail!("Could not find '{path}' on {} (checked project, group, and user)", self.host)
	}

	/// Fetches every project under a namespace: group projects include subgroups, so a
	/// tracked top-level group archives its whole tree.
	pub async fn fetch_namespace_projects(&self, path: &str) -> Result<Vec<Project>> {
		let group = format!("/api/v4/groups/{}?with_projects=false", encode_path(path));
		if self.get_optional::<GroupInfo>(group).await?.is_some() {
			self.fetch_paged(&format!("/api/v4/groups/{}/projects?include_subgroups=true", encode_path(path))).await
		} else {
			self.fetch_paged(&format!("/api/v4/users/{path}/projects")).await
		}
	}

	pub async fn fetch_project(&self, path: &str) -> Result<Project> {
		self.get_optional(format!("/api/v4/projects/{}", encode_path(path)))
			.await?
			.ok_or_else(|| anyhow!("{path} does not exist on {}", self.host))
	}

	/// Follows page-number pagination until a short page. Ordered by id so pages stay
	/// stable if projects are created mid-listing.
	async fn fetch_paged(&self, base: &str) -> Result<Vec<Project>> {
		let sep = if base.contains('?') { '&' } else { '?' };
		let mut all = Vec::new();
		for page in 1u32.. {
			let batch: Vec<Project> =
				self.get(format!("{base}{sep}order_by=id&sort=asc&per_page=100&page={page}")).await?;
			let done = batch.len() < 100;
			all.extend(batch);
			if done {
				break;
			}
		}
		Ok(all)
	}
}

/// The HTTP status of a failed request, when GitLab's error body could be parsed (it usually can:
/// GitLab and GitHub both answer with a JSON `message`).
fn status_of(e: &octocrab::Error) -> Option<u16> {
	match e {
		octocrab::Error::GitHub { source, .. } => Some(source.status_code.as_u16()),
		_ => None,
	}
}

/// A readable message for a failed request. octocrab displays HTTP errors as just "GitHub", so
/// use the message from the response body instead.
fn describe(e: &octocrab::Error) -> String {
	match e {
		octocrab::Error::GitHub { source, .. } => source.message.clone(),
		other => other.to_string(),
	}
}

/// GitLab addresses projects by URL-encoded full path; group and project paths only
/// contain letters, digits, `_`, `-`, and `.`, so only the separators need encoding.
fn encode_path(path: &str) -> String {
	path.replace('/', "%2F")
}

/// Parses an `https://host/path` (or `http://`) argument into `(host, path)`, trimming
/// trailing slashes and a `.git` suffix. Returns `None` for non-URL arguments.
pub fn parse_remote_url(arg: &str) -> Option<(String, String)> {
	let rest = arg.strip_prefix("https://").or_else(|| arg.strip_prefix("http://"))?;
	let (host, path) = rest.split_once('/')?;
	let path = path.trim_end_matches('/');
	let path = path.strip_suffix(".git").unwrap_or(path).trim_end_matches('/');
	if host.is_empty() || path.is_empty() {
		return None;
	}
	Some((host.to_lowercase(), path.to_string()))
}

/// Splits a host-qualified full name (`host/namespace/project`) into host and path.
/// GitHub full names (`owner/repo`) return `None`: GitHub usernames cannot contain
/// dots, while hosts always do, so the first segment cleanly discriminates.
pub fn split_host(full_name: &str) -> Option<(&str, &str)> {
	let (first, rest) = full_name.split_once('/')?;
	(first.contains('.') && !rest.is_empty()).then_some((first, rest))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parse_remote_url_extracts_host_and_path() {
		let (host, path) = parse_remote_url("https://gitlab.example.com/some-group/project").unwrap();
		assert_eq!(host, "gitlab.example.com");
		assert_eq!(path, "some-group/project");
	}

	#[test]
	fn parse_remote_url_trims_git_suffix_and_slash() {
		let (_, path) = parse_remote_url("https://gitlab.example.com/group/repo.git/").unwrap();
		assert_eq!(path, "group/repo");
	}

	#[test]
	fn parse_remote_url_lowercases_host() {
		let (host, _) = parse_remote_url("https://GitLab.Example.COM/group").unwrap();
		assert_eq!(host, "gitlab.example.com");
	}

	#[test]
	fn parse_remote_url_rejects_plain_names() {
		assert!(parse_remote_url("rust-lang").is_none());
		assert!(parse_remote_url("rust-lang/mdBook").is_none());
	}

	#[test]
	fn parse_remote_url_rejects_bare_host() {
		assert!(parse_remote_url("https://gitlab.example.com").is_none());
		assert!(parse_remote_url("https://gitlab.example.com/").is_none());
	}

	#[test]
	fn split_host_recognizes_host_qualified_names() {
		let (host, path) = split_host("gitlab.example.com/some-group/project").unwrap();
		assert_eq!(host, "gitlab.example.com");
		assert_eq!(path, "some-group/project");
	}

	#[test]
	fn split_host_rejects_github_full_names() {
		assert!(split_host("rust-lang/mdBook").is_none());
	}

	#[test]
	fn split_host_rejects_plain_names() {
		assert!(split_host("rust-lang").is_none());
	}

	#[test]
	fn encode_path_escapes_separators_only() {
		assert_eq!(encode_path("group/sub/project"), "group%2Fsub%2Fproject");
		assert_eq!(encode_path("plain"), "plain");
	}
}
