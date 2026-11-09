use anyhow::{Result, bail};

use crate::{config::Config, forge::Target};

/// Freezes or unfreezes tracked accounts, so bulk syncs skip or include them again.
pub fn set_frozen(targets: &[String], frozen: bool) -> Result<()> {
	let mut config = Config::load()?;
	let mut changed = false;
	let mut unknown = 0;
	for arg in targets {
		let target = config.target(arg);
		match set_one(&mut config, &target, frozen) {
			Some(c) => changed |= c,
			None => unknown += 1,
		}
	}
	if changed {
		config.save()?;
	}
	if unknown > 0 {
		bail!("{unknown} of the targets {} not a tracked account", if unknown == 1 { "is" } else { "are" });
	}
	Ok(())
}

/// Sets the frozen flag on one account. Returns whether it changed, or `None` if `target` isn't a
/// tracked account.
fn set_one(config: &mut Config, target: &Target, frozen: bool) -> Option<bool> {
	let key = target.key();
	let host = target.host.as_deref();
	let Some(account) =
		config.track.iter_mut().find(|u| u.host.as_deref() == host && u.name.eq_ignore_ascii_case(&target.path))
	else {
		if config.is_pinned(&key) || config.track.iter().any(|u| u.covers(&key)) {
			println!("{key} is a single repo; only whole accounts can be frozen.");
		} else {
			println!("Not tracking '{key}'.");
		}
		return None;
	};
	let name = account.display_name();
	let verb = if frozen { "frozen" } else { "unfrozen" };
	if account.frozen == frozen {
		println!("{name} is already {verb}.");
		return Some(false);
	}
	account.frozen = frozen;
	if frozen {
		println!("Froze {name}. Bulk syncs will skip it.");
	} else {
		println!("Unfroze {name}. Bulk syncs will include it again.");
	}
	Some(true)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::config::TrackedAccount;

	fn set(config: &mut Config, target: &str, frozen: bool) -> Option<bool> {
		set_one(config, &Target::parse(target), frozen)
	}

	#[test]
	fn freeze_and_unfreeze_ignore_case() {
		let mut config = Config::default();
		config.add_account(None, "Alice", false, false, false, None);
		assert_eq!(set(&mut config, "alice", true), Some(true));
		assert!(config.track[0].frozen);
		assert_eq!(set(&mut config, "ALICE", true), Some(false));
		assert_eq!(set(&mut config, "alice", false), Some(true));
		assert!(!config.track[0].frozen);
	}

	#[test]
	fn freeze_matches_host_and_gitlab_subgroup() {
		let mut config = Config::default();
		config.add_account(None, "grp", false, false, false, None);
		config.track.push(TrackedAccount {
			host: Some("gitlab.example.com".to_string()),
			..TrackedAccount::with_options("grp/sub", false, false)
		});
		assert_eq!(set(&mut config, "https://gitlab.example.com/grp/sub", true), Some(true));
		assert!(config.track.iter().any(|u| u.host.is_some() && u.frozen));
		assert!(config.track.iter().any(|u| u.host.is_none() && !u.frozen), "the GitHub account is untouched");
	}

	#[test]
	fn freeze_rejects_repos_and_untracked_names() {
		let mut config = Config::default();
		config.add_account(None, "alice", false, false, false, None);
		config.pin_repo_with_options("bob/repo", None, None);
		assert_eq!(set(&mut config, "alice/repo", true), None);
		assert_eq!(set(&mut config, "bob/repo", true), None);
		assert_eq!(set(&mut config, "carol", true), None);
		assert!(!config.track[0].frozen);
	}
}
