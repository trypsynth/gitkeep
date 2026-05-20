use std::fs;

use anyhow::Result;

use crate::{config::Config, utils::confirm};

pub fn add(users: &[String], forks: bool, frozen: bool) -> Result<()> {
	let mut config = Config::load()?;
	let mut changed = false;
	for user in users {
		if config.add_user(user, forks, frozen) {
			changed = true;
		}
	}
	if changed {
		config.save()?;
	}
	Ok(())
}

pub fn remove(users: &[String], delete_dir: bool) -> Result<()> {
	let mut config = Config::load()?;
	let mut changed = false;
	let archive_root = config.archive_dir()?;

	for user in users {
		if config.remove_user(user) {
			changed = true;

			let user_dir = archive_root.join(user);
			if user_dir.exists() {
				let should_delete =
					if delete_dir { true } else { confirm(&format!("Delete local archive for {user}?"), false)? };

				if should_delete {
					println!("Deleting {}...", user_dir.display());
					fs::remove_dir_all(&user_dir)?;
				}
			}
		}
	}
	if changed {
		config.save()?;
	}
	Ok(())
}

fn format_list(config: &Config) -> String {
	let mut out = String::new();
	if config.track.is_empty() {
		return "No users tracked. Use 'gitkeep add <username>' to start.\n".to_string();
	}
	out.push_str(&format!("Tracked users and orgs ({} total):\n", config.track.len()));
	for user in &config.track {
		let mut tags = Vec::new();
		if user.forks {
			tags.push("forks");
		}
		if user.frozen {
			tags.push("frozen");
		}
		let suffix = if tags.is_empty() { String::new() } else { format!(" [{}]", tags.join(", ")) };
		out.push_str(&format!("  {}{}\n", user.name, suffix));
	}
	if !config.skipped.is_empty() {
		let mut sorted: Vec<&String> = config.skipped.iter().collect();
		sorted.sort();
		out.push_str(&format!("\nSkipped repos ({} total):\n", sorted.len()));
		for repo in sorted {
			out.push_str(&format!("  {repo}\n"));
		}
	}
	out
}

pub fn list() -> Result<()> {
	let config = Config::load()?;
	print!("{}", format_list(&config));
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn list_empty_state_shows_hint() {
		let config = Config::default();
		let out = format_list(&config);
		assert!(out.contains("gitkeep add"), "got: {out}");
	}

	#[test]
	fn list_shows_tracked_users() {
		let mut config = Config::default();
		config.add_user("alice", false, false);
		let out = format_list(&config);
		assert!(out.contains("alice"), "got: {out}");
	}

	#[test]
	fn list_shows_forks_tag() {
		let mut config = Config::default();
		config.add_user("alice", true, false);
		let out = format_list(&config);
		assert!(out.contains("forks"), "got: {out}");
	}

	#[test]
	fn list_shows_frozen_tag() {
		let mut config = Config::default();
		config.add_user("alice", false, true);
		let out = format_list(&config);
		assert!(out.contains("frozen"), "got: {out}");
	}

	#[test]
	fn list_omits_skipped_section_when_none() {
		let mut config = Config::default();
		config.add_user("alice", false, false);
		let out = format_list(&config);
		assert!(!out.to_lowercase().contains("skipped"), "got: {out}");
	}

	#[test]
	fn list_shows_skipped_section_when_present() {
		let mut config = Config::default();
		config.add_user("alice", false, false);
		config.skip_repo("alice/noisy");
		let out = format_list(&config);
		assert!(out.contains("alice/noisy"), "got: {out}");
		assert!(out.to_lowercase().contains("skipped"), "got: {out}");
	}

	#[test]
	fn list_skipped_repos_are_sorted() {
		let mut config = Config::default();
		config.add_user("alice", false, false);
		config.skip_repo("alice/zzz");
		config.skip_repo("alice/aaa");
		let out = format_list(&config);
		let aaa_pos = out.find("alice/aaa").unwrap();
		let zzz_pos = out.find("alice/zzz").unwrap();
		assert!(aaa_pos < zzz_pos, "got: {out}");
	}
}
