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

pub fn list() -> Result<()> {
	let config = Config::load()?;
	if config.track.is_empty() {
		println!("No users tracked. Use 'gitkeep add <username>' to start.");
		return Ok(());
	}
	println!("Tracked users and orgs ({} total):", config.track.len());
	for user in &config.track {
		let mut tags = Vec::new();
		if user.forks {
			tags.push("forks");
		}
		if user.frozen {
			tags.push("frozen");
		}
		let suffix = if tags.is_empty() { String::new() } else { format!(" [{}]", tags.join(", ")) };
		println!("  {}{}", user.name, suffix);
	}
	Ok(())
}
