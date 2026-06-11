use std::{fmt::Write as _, fs};

use anyhow::{Result, bail};
use octocrab::Octocrab;

use crate::{config::Config, utils::confirm};

pub fn add(users: &[String], forks: bool, frozen: bool) -> Result<()> {
	let mut config = Config::load()?;
	let mut changed = false;
	for user in users {
		if config.add_user(user, forks, frozen) {
			changed = true;
		}
		// Auto-remove any individually-pinned repos from this user — they're now covered.
		for pin in config.remove_pins_for_user(user) {
			println!("{pin} removed from pinned repos (now covered by {user}).");
			changed = true;
		}
	}
	if changed {
		config.save()?;
	}
	Ok(())
}

/// Validates and pins individual repos (`user/repo` format). Returns the canonical names of
/// newly-pinned repos so the caller can sync them.
pub async fn add_pinned(repos: &[String], client: &Octocrab) -> Result<Vec<String>> {
	let mut config = Config::load()?;
	let mut newly_pinned: Vec<String> = Vec::new();

	for repo_str in repos {
		let Some((user, name)) = repo_str.split_once('/') else {
			bail!("'{repo_str}' is not in user/repo format");
		};
		if user.is_empty() || name.is_empty() || name.contains('/') {
			bail!("'{repo_str}' is not in user/repo format");
		}

		// Already tracked in full? No need to pin.
		if let Some(tracked) = config.track.iter().find(|u| u.name.eq_ignore_ascii_case(user)) {
			println!("{} is already fully tracked; {name} will be synced automatically.", tracked.name);
			continue;
		}

		// Case-insensitive duplicate-pin check (before hitting the API).
		if let Some(existing) = config.pinned.iter().find(|p| p.eq_ignore_ascii_case(repo_str)) {
			println!("Already tracking {}.", existing.clone());
			continue;
		}

		// Conflict: repo is currently skipped.
		if config.skipped.iter().any(|s| s.eq_ignore_ascii_case(repo_str)) {
			bail!("'{repo_str}' is currently skipped. Run 'gitkeep unskip {repo_str}' first.");
		}

		// Verify the repo exists on GitHub and get canonical casing.
		let full_name = match client.repos(user, name).get().await {
			Ok(r) => r.full_name.unwrap_or_else(|| repo_str.clone()),
			Err(_) => bail!("'{repo_str}' does not exist on GitHub."),
		};

		// Re-check with canonical name in case casing differed.
		if config.is_pinned(&full_name) {
			println!("Already tracking {full_name}.");
			continue;
		}
		if config.skipped.iter().any(|s| s.eq_ignore_ascii_case(&full_name)) {
			bail!("'{full_name}' is currently skipped. Run 'gitkeep unskip {full_name}' first.");
		}

		config.pin_repo(&full_name);
		println!("Now tracking {full_name}.");
		newly_pinned.push(full_name);
	}

	if !newly_pinned.is_empty() {
		config.save()?;
	}
	Ok(newly_pinned)
}

pub fn remove(users: &[String], delete_dir: bool) -> Result<()> {
	let mut config = Config::load()?;
	let mut changed = false;
	let archive_root = config.archive_dir()?;

	for target in users {
		if target.contains('/') {
			// Repo format: unpin
			if config.unpin_repo(target) {
				println!("No longer tracking {target}.");
				changed = true;

				if delete_dir {
					let Some((user, name)) = target.split_once('/') else { continue };
					let repo_dir = archive_root.join(user).join(name);
					if repo_dir.exists() {
						println!("Deleting {}...", repo_dir.display());
						fs::remove_dir_all(&repo_dir)?;
					}
				}
			} else {
				println!("'{target}' is not pinned.");
			}
		} else {
			// User format: existing behavior
			if config.remove_user(target) {
				changed = true;

				let user_dir = archive_root.join(target);
				if user_dir.exists() {
					let should_delete =
						if delete_dir { true } else { confirm(&format!("Delete local archive for {target}?"), false)? };

					if should_delete {
						println!("Deleting {}...", user_dir.display());
						fs::remove_dir_all(&user_dir)?;
					}
				}
			}
		}
	}
	if changed {
		config.save()?;
	}
	Ok(())
}

fn format_list(config: &Config, archive_dir: Option<&std::path::Path>) -> String {
	let mut out = String::new();
	if config.track.is_empty() && config.pinned.is_empty() {
		return "No users tracked. Use 'gitkeep add <username>' to start.\n".to_string();
	}
	if !config.track.is_empty() {
		let _ = writeln!(out, "Tracked users and orgs ({} total):", config.track.len());
		for user in &config.track {
			let mut tags = Vec::new();
			if user.forks {
				tags.push("forks");
			}
			if user.frozen {
				tags.push("frozen");
			}
			let suffix = if tags.is_empty() { String::new() } else { format!(" [{}]", tags.join(", ")) };
			let _ = writeln!(out, "  {}{}", user.name, suffix);
		}
	}
	if !config.pinned.is_empty() {
		let mut sorted: Vec<&String> = config.pinned.iter().collect();
		sorted.sort();
		if !out.is_empty() {
			out.push('\n');
		}
		let _ = writeln!(out, "Pinned repos ({} total):", sorted.len());
		for repo in sorted {
			let _ = writeln!(out, "  {repo}");
		}
	}
	if !config.skipped.is_empty() {
		let mut sorted: Vec<&String> = config
			.skipped
			.iter()
			.filter(|r| {
				archive_dir
					.as_ref()
					.and_then(|d| r.split_once('/').map(|(u, n)| d.join(u).join(n).exists()))
					.unwrap_or(true)
			})
			.collect();
		sorted.sort();
		if !sorted.is_empty() {
			let _ = writeln!(out, "\nSkipped repos ({} total):", sorted.len());
			for repo in sorted {
				let _ = writeln!(out, "  {repo}");
			}
		}
	}
	out
}

pub fn list() -> Result<()> {
	let config = Config::load()?;
	let archive_dir = config.archive_dir().ok();
	print!("{}", format_list(&config, archive_dir.as_ref().map(|p| p.as_path())));
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn list_empty_state_shows_hint() {
		let config = Config::default();
		let out = format_list(&config, None);
		assert!(out.contains("gitkeep add"), "got: {out}");
	}

	#[test]
	fn list_pinned_only_does_not_show_hint() {
		let mut config = Config::default();
		config.pin_repo("alice/repo");
		let out = format_list(&config, None);
		assert!(!out.contains("gitkeep add"), "got: {out}");
	}

	#[test]
	fn list_shows_tracked_users() {
		let mut config = Config::default();
		config.add_user("alice", false, false);
		let out = format_list(&config, None);
		assert!(out.contains("alice"), "got: {out}");
	}

	#[test]
	fn list_shows_forks_tag() {
		let mut config = Config::default();
		config.add_user("alice", true, false);
		let out = format_list(&config, None);
		assert!(out.contains("forks"), "got: {out}");
	}

	#[test]
	fn list_shows_frozen_tag() {
		let mut config = Config::default();
		config.add_user("alice", false, true);
		let out = format_list(&config, None);
		assert!(out.contains("frozen"), "got: {out}");
	}

	#[test]
	fn list_omits_skipped_section_when_none() {
		let mut config = Config::default();
		config.add_user("alice", false, false);
		let out = format_list(&config, None);
		assert!(!out.to_lowercase().contains("skipped"), "got: {out}");
	}

	#[test]
	fn list_shows_skipped_section_when_present() {
		let mut config = Config::default();
		config.add_user("alice", false, false);
		config.skip_repo("alice/noisy");
		let out = format_list(&config, None);
		assert!(out.contains("alice/noisy"), "got: {out}");
		assert!(out.to_lowercase().contains("skipped"), "got: {out}");
	}

	#[test]
	fn list_skipped_repos_are_sorted() {
		let mut config = Config::default();
		config.add_user("alice", false, false);
		config.skip_repo("alice/zzz");
		config.skip_repo("alice/aaa");
		let out = format_list(&config, None);
		let aaa_pos = out.find("alice/aaa").unwrap();
		let zzz_pos = out.find("alice/zzz").unwrap();
		assert!(aaa_pos < zzz_pos, "got: {out}");
	}

	#[test]
	fn list_shows_pinned_section() {
		let mut config = Config::default();
		config.pin_repo("rust-lang/mdBook");
		let out = format_list(&config, None);
		assert!(out.contains("rust-lang/mdBook"), "got: {out}");
		assert!(out.to_lowercase().contains("pinned"), "got: {out}");
	}

	#[test]
	fn list_pinned_repos_are_sorted() {
		let mut config = Config::default();
		config.pin_repo("rust-lang/zzz");
		config.pin_repo("rust-lang/aaa");
		let out = format_list(&config, None);
		let aaa_pos = out.find("rust-lang/aaa").unwrap();
		let zzz_pos = out.find("rust-lang/zzz").unwrap();
		assert!(aaa_pos < zzz_pos, "got: {out}");
	}

	#[test]
	fn list_omits_pinned_section_when_none() {
		let mut config = Config::default();
		config.add_user("alice", false, false);
		let out = format_list(&config, None);
		assert!(!out.to_lowercase().contains("pinned"), "got: {out}");
	}

	#[test]
	fn add_user_removes_pins_for_that_user() {
		let mut config = Config::default();
		config.pin_repo("alice/foo");
		config.pin_repo("alice/bar");
		config.pin_repo("bob/baz");
		config.add_user("alice", false, false);
		let pins_removed = config.remove_pins_for_user("alice");
		assert_eq!(pins_removed.len(), 2);
		assert!(config.is_pinned("bob/baz"));
	}
}
