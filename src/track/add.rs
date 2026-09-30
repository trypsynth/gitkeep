use std::collections::{HashMap, hash_map::Entry};

use anyhow::Result;

use crate::{
	config::{Config, HostConfig, TrackedAccount},
	forge::{self, Forge, Resolved, Target},
};

/// What `add` recorded, by key, so the caller can sync it right away.
#[derive(Default)]
pub struct Added {
	pub accounts: Vec<String>,
	pub pinned: Vec<String>,
	/// Tracked accounts that had a previously removed repo added back.
	pub restored_owners: Vec<String>,
}

/// Adds accounts and repos from any forge. Each target is a GitHub name (`owner` or
/// `owner/repo`), a host-qualified name (`host/path`), or a URL. The first time a host is seen, its
/// forge kind is detected and recorded.
pub async fn add(targets: &[String], forks: bool, frozen: bool, submodules: Option<bool>) -> Result<Added> {
	let mut config = Config::load()?;
	let mut changed = false;
	let mut forges: HashMap<Option<String>, Forge> = HashMap::new();
	let mut resolved = Vec::with_capacity(targets.len());
	for arg in targets {
		let target = Target::parse(arg);
		if let Some(host) = &target.host
			&& !config.hosts.contains_key(host)
		{
			let kind = forge::detect(host).await?;
			config.hosts.insert(host.clone(), HostConfig { kind, token: None });
			changed = true;
		}
		let forge = match forges.entry(target.host.clone()) {
			Entry::Occupied(e) => e.into_mut(),
			Entry::Vacant(e) => e.insert(config.forge(target.host.as_deref())?),
		};
		resolved.push((target.host, forge.resolve(&target.path).await?));
	}
	let mut added = Added::default();
	// Record accounts before repos, so `gitkeep add rust-lang rust-lang/mdBook` tracks the org and
	// skips the now-redundant pin.
	for (host, item) in &resolved {
		if let Resolved::Account(name) = item {
			changed |= config.add_account(host.as_deref(), name, forks, frozen, submodules);
			let tracked = TrackedAccount { host: host.clone(), ..TrackedAccount::with_options(name, forks, frozen) };
			for pin in config.remove_pins_covered(&tracked) {
				println!("{pin} removed (now covered by {}).", tracked.display_name());
				changed = true;
			}
			added.accounts.push(tracked.display_name());
		}
	}
	for (_, item) in resolved {
		let Resolved::Repo(repo) = item else { continue };
		let key = repo.full_name;
		if let Some(owner) = config.track.iter().find(|u| u.covers(&key)).map(TrackedAccount::display_name) {
			if let Some(restored) = config.include_repo(&key) {
				println!("Now tracking {restored} again.");
				changed = true;
				if !added.restored_owners.contains(&owner) {
					added.restored_owners.push(owner);
				}
			} else {
				println!("{owner} is already fully tracked; {key} will be synced automatically.");
			}
		} else if config.is_pinned(&key) {
			println!("Already tracking {key}.");
		} else {
			// A leftover exclusion for an account that's no longer tracked means nothing; drop it.
			config.include_repo(&key);
			config.pin_repo_with_options(&key, Some(repo.id), submodules);
			println!("Now tracking {key}.");
			added.pinned.push(key);
			changed = true;
		}
	}
	if changed {
		config.save()?;
	}
	Ok(added)
}
