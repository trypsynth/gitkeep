use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use octocrab::{Octocrab, models::Repository};
use serde::Deserialize;
use tokio::sync::OnceCell;

use super::{AccountRepos, RemoteRepo, Resolved};

#[derive(Deserialize)]
struct AccountInfo {
	login: String,
	id: u64,
	#[serde(rename = "type")]
	account_type: String,
}

/// github.com, through octocrab. `authenticated` records whether a token is configured, which
/// decides whether private repos can be listed.
pub struct GitHub {
	client: Octocrab,
	authenticated: bool,
	/// Every repo the token can see. It's the same list whichever account is being synced, so it's
	/// only fetched once.
	accessible: OnceCell<Vec<Repository>>,
}

impl GitHub {
	pub fn new(client: Octocrab, authenticated: bool) -> Self {
		Self { client, authenticated, accessible: OnceCell::new() }
	}

	/// Resolves an `add` target: a bare name is an account, `owner/repo` a single repo.
	pub async fn resolve(&self, path: &str) -> Result<Resolved> {
		match path.split_once('/') {
			None => {
				let info: AccountInfo = self
					.client
					.get(format!("/users/{path}"), None::<&()>)
					.await
					.with_context(|| format!("Could not find GitHub user '{path}'"))?;
				Ok(Resolved::Account(info.login))
			}
			Some((owner, name)) if !owner.is_empty() && !name.is_empty() && !name.contains('/') => {
				let Some(repo) = self.repo(path, None).await? else { bail!("'{path}' does not exist on GitHub.") };
				Ok(Resolved::Repo(repo))
			}
			_ => bail!("'{path}' is not in user/repo format"),
		}
	}

	/// Lists an account's repos. If the name no longer resolves, the stored `id` is tried, since it
	/// still finds the account after any number of renames; the returned name is the current login.
	pub async fn account(&self, name: &str, id: Option<u64>) -> Result<AccountRepos> {
		let mut account = self.client.get::<AccountInfo, _, _>(format!("/users/{name}"), None::<&()>).await.ok();
		if account.is_none()
			&& let Some(id) = id
		{
			account = self.client.get(format!("/user/{id}"), None::<&()>).await.ok();
		}
		let login = account.as_ref().map_or_else(|| name.to_string(), |a| a.login.clone());
		let is_org = account.as_ref().is_some_and(|a| a.account_type == "Organization");
		let repos = match (is_org, self.authenticated) {
			(true, true) => self.fetch_org(&login).await?.iter().map(|r| remote_repo(r, &login)).collect(),
			(false, true) => self.fetch_with_token(&login).await?,
			(_, false) => self.fetch_public(&login).await?.iter().map(|r| remote_repo(r, &login)).collect(),
		};
		Ok(AccountRepos { name: login, id: account.map(|a| a.id), repos })
	}

	/// Fetches one repo by `owner/repo`, falling back to its stable `id` if it or its owner was
	/// renamed. `None` if neither finds it.
	pub async fn repo(&self, path: &str, id: Option<u64>) -> Result<Option<RemoteRepo>> {
		let Some((owner, name)) = path.split_once('/') else { return Ok(None) };
		let mut repo = self.client.repos(owner, name).get().await.ok();
		if repo.is_none()
			&& let Some(id) = id
		{
			repo = self.client.get(format!("/repositories/{id}"), None::<&()>).await.ok();
		}
		Ok(repo.map(|r| {
			let owner = r.full_name.as_deref().and_then(|f| f.split_once('/')).map_or(owner, |(o, _)| o).to_string();
			remote_repo(&r, &owner)
		}))
	}

	/// An account's public repos plus any others of its that the token can see.
	async fn fetch_with_token(&self, username: &str) -> Result<Vec<RemoteRepo>> {
		let (public, accessible) = tokio::try_join!(self.fetch_public(username), self.fetch_accessible())?;
		let owned =
			accessible.iter().filter(|r| r.owner.as_ref().is_some_and(|o| o.login.eq_ignore_ascii_case(username)));
		let mut seen = HashSet::new();
		Ok(public.iter().chain(owned).filter(|r| seen.insert(r.id)).map(|r| remote_repo(r, username)).collect())
	}

	async fn fetch_accessible(&self) -> Result<&Vec<Repository>> {
		self.accessible
			.get_or_try_init(|| async {
				let page = self
					.client
					.current()
					.list_repos_for_authenticated_user()
					.per_page(100u8)
					.send()
					.await
					.context("Could not fetch your repositories from GitHub")?;
				self.client.all_pages(page).await.context("Could not retrieve all repository pages")
			})
			.await
	}

	async fn fetch_org(&self, org: &str) -> Result<Vec<Repository>> {
		let page = self
			.client
			.orgs(org)
			.list_repos()
			.per_page(100u8)
			.send()
			.await
			.with_context(|| format!("Could not fetch repositories for org {org}"))?;
		self.client
			.all_pages(page)
			.await
			.with_context(|| format!("Could not retrieve all repository pages for org {org}"))
	}

	async fn fetch_public(&self, username: &str) -> Result<Vec<Repository>> {
		let page = self
			.client
			.users(username)
			.repos()
			.per_page(100u8)
			.send()
			.await
			.with_context(|| format!("Could not fetch public repositories for {username}"))?;
		self.client
			.all_pages(page)
			.await
			.with_context(|| format!("Could not retrieve all repository pages for {username}"))
	}
}

/// Converts an octocrab repo, stored under `owner` (the account's canonical login).
fn remote_repo(repo: &Repository, owner: &str) -> RemoteRepo {
	RemoteRepo {
		full_name: repo.full_name.clone().unwrap_or_else(|| format!("{owner}/{}", repo.name)),
		rel_dir: format!("{owner}/{}", repo.name),
		id: Some(repo.id.into_inner()),
		pushed_at: repo.pushed_at,
		fork: repo.fork.unwrap_or(false),
		clone_url: repo.clone_url.as_ref().map(ToString::to_string),
		ssh_url: repo.ssh_url.clone(),
	}
}

#[cfg(test)]
mod tests {
	use std::{
		io::{Read as _, Write as _},
		net::TcpListener,
		sync::{
			Arc,
			atomic::{AtomicUsize, Ordering},
		},
		thread,
	};

	use octocrab::OctocrabBuilder;

	use super::*;

	fn repo_json(id: u64, owner: &str, name: &str) -> String {
		format!(
			r#"{{"id":{id},"name":"{name}","full_name":"{owner}/{name}","url":"https://api.github.com/repos/{owner}/{name}",
			"owner":{{"login":"{owner}","id":1,"node_id":"x","avatar_url":"https://a.test/a","gravatar_id":"",
			"url":"https://a.test/u","html_url":"https://a.test/h","followers_url":"https://a.test/f",
			"following_url":"https://a.test/f","gists_url":"https://a.test/g","starred_url":"https://a.test/s",
			"subscriptions_url":"https://a.test/s","organizations_url":"https://a.test/o","repos_url":"https://a.test/r",
			"events_url":"https://a.test/e","received_events_url":"https://a.test/r","type":"User","site_admin":false}}}}"#
		)
	}

	/// Serves canned GitHub responses on a local port, counting requests for the token's repo list.
	fn mock_github() -> (String, Arc<AtomicUsize>) {
		let listener = TcpListener::bind("127.0.0.1:0").unwrap();
		let url = format!("http://{}", listener.local_addr().unwrap());
		let hits = Arc::new(AtomicUsize::new(0));
		let counter = Arc::clone(&hits);
		thread::spawn(move || {
			for stream in listener.incoming() {
				let mut stream = stream.unwrap();
				let mut buf = [0; 4096];
				let n = stream.read(&mut buf).unwrap();
				let request = String::from_utf8_lossy(&buf[..n]);
				let path = request.split_whitespace().nth(1).unwrap_or_default();
				let body = if path.starts_with("/user/repos") {
					counter.fetch_add(1, Ordering::SeqCst);
					format!("[{},{}]", repo_json(1, "alice", "private"), repo_json(2, "bob", "shared"))
				} else if let Some(rest) = path.strip_prefix("/users/") {
					match rest.split_once('/') {
						Some(("alice", _)) => format!("[{}]", repo_json(1, "alice", "private")),
						Some(_) => "[]".to_string(),
						None => {
							let login = rest.split('?').next().unwrap_or_default();
							format!(r#"{{"login":"{login}","id":9,"type":"User"}}"#)
						}
					}
				} else {
					"{}".to_string()
				};
				let response = format!(
					"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
					body.len()
				);
				stream.write_all(response.as_bytes()).unwrap();
			}
		});
		(url, hits)
	}

	#[tokio::test]
	async fn accessible_repos_are_fetched_once_and_split_by_owner() {
		let (url, hits) = mock_github();
		let client = OctocrabBuilder::default().base_uri(url).unwrap().build().unwrap();
		let github = GitHub::new(client, true);
		let (alice, bob) = tokio::join!(github.account("alice", None), github.account("bob", None));
		let names = |repos: &[RemoteRepo]| repos.iter().map(|r| r.full_name.clone()).collect::<Vec<_>>();
		assert_eq!(names(&alice.unwrap().repos), ["alice/private"], "the public and accessible copies merge");
		assert_eq!(names(&bob.unwrap().repos), ["bob/shared"]);
		assert_eq!(hits.load(Ordering::SeqCst), 1);
	}
}
