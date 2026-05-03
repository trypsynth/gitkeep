use anyhow::Result;

use crate::config::{Config, TrackedUser};

fn plural(n: usize, singular: &str, plural: &str) -> String {
	if n == 1 { format!("1 {singular}") } else { format!("{n} {plural}") }
}

pub fn add(users: &[String], forks: bool) -> Result<()> {
	let mut config = Config::load()?;
	let mut changed = false;
	for user in users {
		if let Some(entry) = config.track.iter_mut().find(|u| &u.name == user) {
			if forks && !entry.forks {
				entry.forks = true;
				println!("Forks enabled for {}.", user);
				changed = true;
			} else {
				println!("Already tracking {}.", user);
			}
		} else {
			let entry = if forks { TrackedUser::with_forks(user) } else { TrackedUser::new(user) };
			println!("Now tracking {}{}", user, if forks { " (forks included)" } else { "" });
			config.track.push(entry);
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
	let mut removed = 0usize;
	for user in users {
		let before = config.track.len();
		config.track.retain(|u| u.name != user.as_str());
		if config.track.len() < before {
			println!("Stopped tracking {}.", user);
			removed += 1;
		} else {
			println!("Not tracking {}.", user);
		}
	}
	if removed > 0 {
		config.save()?;
		println!("Removed {}.", plural(removed, "user", "users"));
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
		if user.forks {
			println!("  {} [forks]", user.name);
		} else {
			println!("  {}", user.name);
		}
	}
	Ok(())
}
