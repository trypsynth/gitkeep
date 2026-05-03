use anyhow::Result;

use crate::config::Config;

pub fn add(users: &[String], forks: bool) -> Result<()> {
	let mut config = Config::load()?;
	let mut changed = false;
	for user in users {
		if config.add_user(user, forks) {
			changed = true;
		}
	}
	if changed {
		config.save()?;
	}
	Ok(())
}

pub fn remove(users: &[String]) -> Result<()> {
	let mut config = Config::load()?;
	let mut changed = false;
	for user in users {
		if config.remove_user(user) {
			changed = true;
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
		let suffix = if user.forks { " [forks]" } else { "" };
		println!("  {}{}", user.name, suffix);
	}
	Ok(())
}
