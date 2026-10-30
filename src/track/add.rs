use std::{
	collections::{HashMap, hash_map::Entry},
	io::{self, IsTerminal as _},
};

use anyhow::Result;

use crate::{
	config::{Config, HostConfig, TrackedAccount},
	forge::{self, Forge, Resolved, Target},
	utils::confirm,
};

/// What `add` recorded, in the order it was given, so the caller can sync it right away.
#[derive(Default)]
pub struct Added {
	pub items: Vec<AddedItem>,
}

/// One thing `add` recorded, by key.
#[derive(Debug, PartialEq, Eq)]
pub enum AddedItem {
	Account(String),
	/// A tracked account that had a previously removed repo added back.
	Restored(String),
	Pin(String),
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
			// Scripts get the global setting; only a person at a terminal is asked. An answer that
			// matches the global setting isn't stored, so the host keeps following it if it changes.
			let use_ssh = if io::stdin().is_terminal() {
				Some(confirm(&format!("Use SSH clone URLs for {host}?"), config.use_ssh)?)
					.filter(|&ssh| ssh != config.use_ssh)
			} else {
				None
			};
			config.hosts.insert(host.clone(), HostConfig { kind, token: None, use_ssh });
			changed = true;
		}
		let forge = match forges.entry(target.host.clone()) {
			Entry::Occupied(e) => e.into_mut(),
			Entry::Vacant(e) => e.insert(config.forge(target.host.as_deref())?),
		};
		resolved.push((target.host, forge.resolve(&target.path).await?));
	}
	let mut added = Added::default();
	for (host, item) in resolved {
		match item {
			Resolved::Account(name) => {
				changed |= config.add_account(host.as_deref(), &name, forks, frozen, submodules);
				let tracked = TrackedAccount { host, ..TrackedAccount::with_options(&name, forks, frozen) };
				// A repo given earlier in the same command is dropped here too, so it isn't synced twice.
				for pin in config.remove_pins_covered(&tracked) {
					println!("{pin} removed (now covered by {}).", tracked.display_name());
					added.items.retain(|i| *i != AddedItem::Pin(pin.clone()));
					changed = true;
				}
				added.items.push(AddedItem::Account(tracked.display_name()));
			}
			Resolved::Repo(repo) => {
				let key = repo.full_name;
				if let Some(owner) = config.track.iter().find(|u| u.covers(&key)).map(TrackedAccount::display_name) {
					if let Some(restored) = config.include_repo(&key) {
						println!("Now tracking {restored} again.");
						changed = true;
						let owner = AddedItem::Restored(owner);
						if !added.items.contains(&owner) {
							added.items.push(owner);
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
					added.items.push(AddedItem::Pin(key));
					changed = true;
				}
			}
		}
	}
	if changed {
		config.save()?;
	}
	Ok(added)
}
