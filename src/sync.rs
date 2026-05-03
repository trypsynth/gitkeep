use std::{collections::HashSet, fs, path::Path, process::Command};

use anyhow::{Context, Result, bail};
use octocrab::{Octocrab, OctocrabBuilder, models::Repository};
use serde::Deserialize;

use crate::{
	config::{Config, State, TrackedUser},
	utils::plural,
};

#[derive(Default)]
struct Totals {
	synced: usize,
	skipped: usize,
	failed: usize,
}

pub async fn run(extra_users: &[String], force_forks: bool) -> Result<()> {
	let mut config = Config::load().context("Could not load config")?;
	let client = build_client(&config)?;
	let mut updated = false;
	for user in extra_users {
		if !config.is_tracked(user) {
			println!("Adding {user} to tracked users.");
			config.track.push(TrackedUser::new(user));
			updated = true;
		}
	}
	if updated {
		config.save().context("Could not update config")?;
	}
	if config.track.is_empty() {
		bail!(
			"Nothing to sync. Use 'gitkeep add <username>' to start building your library, \
             or run 'gitkeep login' to authenticate and auto-add your account."
		);
	}
	let archive_dir = config.archive_dir()?;
	fs::create_dir_all(&archive_dir)
		.with_context(|| format!("Could not create archive directory: {}", archive_dir.display()))?;
	let mut state = State::load()?;
	let mut totals = Totals::default();
	let mut seen = HashSet::new();
	let to_sync: Vec<&TrackedUser> = config.track.iter().filter(|u| seen.insert(u.name.as_str())).collect();
	for user in to_sync {
		sync_one(user, force_forks, &client, &archive_dir, &config, &mut state, &mut totals).await;
	}
	state.save().context("Could not save sync state")?;
	println!("Sync complete. {} synced, {} skipped, {} failed.", totals.synced, totals.skipped, totals.failed);
	Ok(())
}

pub async fn run_for(targets: &[String], force_forks: bool) -> Result<()> {
	let config = Config::load().context("Could not load config")?;
	let client = build_client(&config)?;
	let archive_dir = config.archive_dir()?;
	fs::create_dir_all(&archive_dir)
		.with_context(|| format!("Could not create archive directory: {}", archive_dir.display()))?;
	let mut state = State::load()?;
	let mut totals = Totals::default();
	let mut seen = HashSet::new();
	let to_sync: Vec<&TrackedUser> = config
		.track
		.iter()
		.filter(|u| targets.iter().any(|t| t.eq_ignore_ascii_case(&u.name)))
		.filter(|u| seen.insert(u.name.as_str()))
		.collect();
	for user in to_sync {
		sync_one(user, force_forks, &client, &archive_dir, &config, &mut state, &mut totals).await;
	}
	state.save().context("Could not save sync state")?;
	println!("Sync complete. {} synced, {} skipped, {} failed.", totals.synced, totals.skipped, totals.failed);
	Ok(())
}

async fn sync_one(
	user: &TrackedUser,
	force_forks: bool,
	client: &Octocrab,
	archive_dir: &Path,
	config: &Config,
	state: &mut State,
	totals: &mut Totals,
) {
	println!("Checking {}...", user.name);
	let repos_result = if account_is_org(client, &user.name).await {
		if config.token.is_some() {
			fetch_org(client, &user.name).await
		} else {
			fetch_public(client, &user.name).await
		}
	} else if config.token.is_some() {
		fetch_with_token(client, &user.name).await
	} else {
		fetch_public(client, &user.name).await
	};
	match repos_result {
		Ok(repos) => {
			let include_forks = force_forks || user.forks;
			let fork_count = repos.iter().filter(|r| r.fork.unwrap_or(false)).count();
			let visible = repos.len() - if include_forks { 0 } else { fork_count };

			let mut msg = format!("Found {} for {}.", plural(visible, "repository", "repositories"), user.name);
			if !include_forks && fork_count > 0 {
				msg.push_str(&format!(
					" Skipping {}. Use 'gitkeep add --forks {}' to include them.",
					plural(fork_count, "fork", "forks"),
					user.name
				));
			}
			println!("{msg}");

			sync_repo_list(repos, &user.name, include_forks, archive_dir, config, state, totals);
		}
		Err(e) => {
			eprintln!("  Could not fetch repositories for {}: {e:#}.", user.name);
			totals.failed += 1;
		}
	}
}

fn sync_repo_list(
	repos: Vec<Repository>,
	username: &str,
	include_forks: bool,
	archive_dir: &Path,
	config: &Config,
	state: &mut State,
	totals: &mut Totals,
) {
	let user_dir = archive_dir.join(username);
	if let Err(e) = fs::create_dir_all(&user_dir) {
		eprintln!("  Could not create directory for {username}: {e}.");
		totals.failed += repos.len();
		return;
	}
	for repo in repos {
		let name = &repo.name;
		let full_name = repo.full_name.as_deref().unwrap_or(name.as_str());
		if repo.fork.unwrap_or(false) && !include_forks {
			totals.skipped += 1;
			continue;
		}
		let Some(url) = clone_url(&repo, config.use_ssh) else {
			println!("Skipping {name} (no clone URL available).");
			totals.skipped += 1;
			continue;
		};
		let repo_dir = user_dir.join(name.as_str());
		let result = if repo_dir.exists() {
			println!("Pulling {username}/{name}...");
			git_pull(&repo_dir)
		} else {
			println!("Cloning {username}/{name}...");
			git_clone(&url, &repo_dir)
		};
		match result {
			Ok(()) => {
				state.mark_synced(full_name);
				totals.synced += 1;
			}
			Err(e) => {
				eprintln!("  Failed: {name}: {e:#}.");
				totals.failed += 1;
			}
		}
	}
}

fn build_client(config: &Config) -> Result<Octocrab> {
	if let Some(token) = &config.token {
		OctocrabBuilder::default()
			.personal_token(token.clone())
			.build()
			.context("Could not create authenticated GitHub client")
	} else {
		println!("Warning: Running in unauthenticated mode. Rate limits will be restricted.");
		OctocrabBuilder::default().build().context("Could not create GitHub client")
	}
}

async fn fetch_with_token(client: &Octocrab, username: &str) -> Result<Vec<Repository>> {
	let (public, accessible) = tokio::try_join!(fetch_public(client, username), fetch_authenticated(client, username))?;
	let mut seen = HashSet::new();
	let mut merged = Vec::with_capacity(public.len() + accessible.len());
	for repo in public.into_iter().chain(accessible) {
		if seen.insert(repo.id) {
			merged.push(repo);
		}
	}
	Ok(merged)
}

async fn fetch_authenticated(client: &Octocrab, username: &str) -> Result<Vec<Repository>> {
	let page = client
		.current()
		.list_repos_for_authenticated_user()
		.per_page(100u8)
		.send()
		.await
		.context("Could not fetch your repositories from GitHub")?;
	let repos = client.all_pages(page).await.context("Could not retrieve all repository pages")?;
	Ok(repos.into_iter().filter(|r| r.owner.as_ref().is_some_and(|o| o.login.eq_ignore_ascii_case(username))).collect())
}

#[derive(Deserialize)]
struct AccountInfo {
	#[serde(rename = "type")]
	account_type: String,
}

async fn account_is_org(client: &Octocrab, name: &str) -> bool {
	let result: Result<AccountInfo, _> = client.get(format!("/users/{name}"), None::<&()>).await;
	result.is_ok_and(|info| info.account_type == "Organization")
}

async fn fetch_org(client: &Octocrab, org: &str) -> Result<Vec<Repository>> {
	let page = client
		.orgs(org)
		.list_repos()
		.per_page(100u8)
		.send()
		.await
		.with_context(|| format!("Could not fetch repositories for org {org}"))?;
	client.all_pages(page).await.with_context(|| format!("Could not retrieve all repository pages for org {org}"))
}

async fn fetch_public(client: &Octocrab, username: &str) -> Result<Vec<Repository>> {
	let page = client
		.users(username)
		.repos()
		.per_page(100u8)
		.send()
		.await
		.with_context(|| format!("Could not fetch public repositories for {username}"))?;
	client.all_pages(page).await.with_context(|| format!("Could not retrieve all repository pages for {username}"))
}

fn clone_url(repo: &Repository, use_ssh: bool) -> Option<String> {
	if use_ssh { repo.ssh_url.clone() } else { repo.clone_url.as_ref().map(std::string::ToString::to_string) }
}

fn git_clone(url: &str, dest: &Path) -> Result<()> {
	let status = Command::new("git")
		.args(["clone", "--", url])
		.arg(dest)
		.status()
		.context("Could not run 'git clone'. Is git installed and on your PATH?")?;
	if !status.success() {
		bail!("git clone failed with code {}. Check the URL and your credentials.", status.code().unwrap_or(-1));
	}
	Ok(())
}

fn git_pull(repo_dir: &Path) -> Result<()> {
	let status = Command::new("git")
		.arg("pull")
		.current_dir(repo_dir)
		.status()
		.context("Could not run 'git pull'. Is git installed and on your PATH?")?;
	if !status.success() {
		bail!("git pull failed with code {} in {}.", status.code().unwrap_or(-1), repo_dir.display());
	}
	Ok(())
}
