use anyhow::{Result, bail};

use crate::config::Config;

fn parse_repo_arg(s: &str) -> Result<(&str, &str)> {
	let Some((user, rest)) = s.split_once('/') else {
		bail!("'{s}' is not in user/repo format");
	};
	if user.is_empty() {
		bail!("'{s}' is not in user/repo format");
	}
	if rest.is_empty() || rest.contains('/') {
		bail!("'{s}' is not in user/repo format");
	}
	Ok((user, rest))
}

pub async fn add(repos: &[String]) -> Result<()> {
	let mut config = Config::load()?;
	let client = config.build_client()?;
	let archive_dir = config.archive_dir()?;

	for repo_str in repos {
		let (user, name) = parse_repo_arg(repo_str)?;

		let local_path = archive_dir.join(user).join(name);
		if local_path.exists() {
			let canonical_user =
				config.track.iter().find(|u| u.name.eq_ignore_ascii_case(user)).map_or(user, |u| u.name.as_str());
			let key = format!("{canonical_user}/{name}");
			if config.skip_repo(&key) {
				println!("Skipping {key}.");
			} else {
				println!("Already skipping {key}. Run 'gitkeep unskip {key}' to stop.");
			}
			continue;
		}

		let user_tracked = config.track.iter().any(|u| u.name.eq_ignore_ascii_case(user));
		if !user_tracked {
			bail!(
				"'{repo_str}' won't be synced: '{user}' is not in your tracked list. \
				 Run 'gitkeep add {user}' first."
			);
		}

		let github_repo = client.repos(user, name).get().await;
		let full_name = match github_repo {
			Ok(r) => r.full_name.unwrap_or_else(|| repo_str.clone()),
			Err(_) => bail!("'{repo_str}' does not exist on GitHub."),
		};

		if config.skip_repo(&full_name) {
			println!("Skipping {full_name}.");
		} else {
			println!("Already skipping {full_name}. Run 'gitkeep unskip {full_name}' to stop.");
		}
	}

	config.save()
}

fn unskip_one(config: &mut Config, repo: &str) -> Result<()> {
	parse_repo_arg(repo)?;
	if config.unskip_repo(repo) {
		println!("No longer skipping {repo}.");
	} else {
		println!("'{repo}' is not currently skipped.");
	}
	Ok(())
}

pub fn remove(repos: &[String]) -> Result<()> {
	let mut config = Config::load()?;
	for repo in repos {
		unskip_one(&mut config, repo)?;
	}
	config.save()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parse_repo_arg_valid() {
		let (user, name) = parse_repo_arg("alice/my-repo").unwrap();
		assert_eq!(user, "alice");
		assert_eq!(name, "my-repo");
	}

	#[test]
	fn parse_repo_arg_rejects_no_slash() {
		assert!(parse_repo_arg("noslash").is_err());
	}

	#[test]
	fn parse_repo_arg_rejects_two_slashes() {
		assert!(parse_repo_arg("a/b/c").is_err());
	}

	#[test]
	fn parse_repo_arg_rejects_empty_user() {
		assert!(parse_repo_arg("/repo").is_err());
	}

	#[test]
	fn parse_repo_arg_rejects_empty_repo() {
		assert!(parse_repo_arg("user/").is_err());
	}

	#[test]
	fn unskip_one_rejects_bad_format() {
		let mut config = Config::default();
		assert!(unskip_one(&mut config, "noslash").is_err());
	}

	#[test]
	fn unskip_one_succeeds_when_not_skipped() {
		let mut config = Config::default();
		// should succeed (not error) even if not currently skipped
		assert!(unskip_one(&mut config, "user/repo").is_ok());
	}

	#[test]
	fn unskip_one_removes_existing_skip() {
		let mut config = Config::default();
		config.skip_repo("user/repo");
		unskip_one(&mut config, "user/repo").unwrap();
		assert!(!config.is_skipped("user/repo"));
	}
}
