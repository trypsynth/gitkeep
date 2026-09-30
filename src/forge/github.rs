use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use octocrab::{Octocrab, models::Repository};
use serde::Deserialize;

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
}

impl GitHub {
	pub const fn new(client: Octocrab, authenticated: bool) -> Self {
		Self { client, authenticated }
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
			(true, true) => self.fetch_org(&login).await?,
			(false, true) => self.fetch_with_token(&login).await?,
			(_, false) => self.fetch_public(&login).await?,
		};
		let repos = repos.iter().map(|r| remote_repo(r, &login)).collect();
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

	async fn fetch_with_token(&self, username: &str) -> Result<Vec<Repository>> {
		let (public, accessible) = tokio::try_join!(self.fetch_public(username), self.fetch_authenticated(username))?;
		let mut seen = HashSet::new();
		let mut merged = Vec::with_capacity(public.len() + accessible.len());
		for repo in public.into_iter().chain(accessible) {
			if seen.insert(repo.id) {
				merged.push(repo);
			}
		}
		Ok(merged)
	}

	async fn fetch_authenticated(&self, username: &str) -> Result<Vec<Repository>> {
		let page = self
			.client
			.current()
			.list_repos_for_authenticated_user()
			.per_page(100u8)
			.send()
			.await
			.context("Could not fetch your repositories from GitHub")?;
		let repos = self.client.all_pages(page).await.context("Could not retrieve all repository pages")?;
		Ok(repos
			.into_iter()
			.filter(|r| r.owner.as_ref().is_some_and(|o| o.login.eq_ignore_ascii_case(username)))
			.collect())
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
		id: repo.id.into_inner(),
		pushed_at: repo.pushed_at,
		fork: repo.fork.unwrap_or(false),
		clone_url: repo.clone_url.as_ref().map(ToString::to_string),
		ssh_url: repo.ssh_url.clone(),
	}
}
