use std::collections::{HashMap, hash_map::Entry};

use anyhow::{Result, bail};

use crate::{
	config::{Config, HostConfig, TrackedAccount},
	forge::{self, Forge, ForgeKind, Gitolite, Resolved, Target},
	utils::confirm,
};

/// One thing `add` recorded, by key. `add` returns them in the order they were given, so the
/// caller can sync them right away.
#[derive(Debug, PartialEq, Eq)]
pub enum AddedItem {
	Account(String),
	/// A tracked account that had a previously removed repo added back.
	Restored(String),
	Pin(String),
}

/// Adds accounts and repos from any forge. Each target is a GitHub name (`owner` or
/// `owner/repo`), a host-qualified name (`host/path`), a URL, or an SSH alias and path
/// (`alias:path`). The first time a host is seen, its forge kind is detected and recorded.
pub async fn add(targets: &[String], forks: bool, frozen: bool, submodules: Option<bool>) -> Result<Vec<AddedItem>> {
	let mut config = Config::load()?;
	let mut changed = false;
	let mut forges: HashMap<Option<String>, Forge> = HashMap::new();
	let mut resolved = Vec::with_capacity(targets.len());
	for arg in targets {
		let mut target = config.target(arg);
		changed |= register_host(&mut config, &mut target).await?;
		let forge = match forges.entry(target.host.clone()) {
			Entry::Occupied(e) => e.into_mut(),
			Entry::Vacant(e) => e.insert(config.forge(target.host.as_deref())?),
		};
		resolved.push((target.host, forge.resolve(&target.path).await?));
	}
	let mut added = Vec::new();
	for (host, item) in resolved {
		match item {
			Resolved::Account(name) => {
				changed |= config.add_account(host.as_deref(), &name, forks, frozen, submodules);
				let tracked = TrackedAccount { host, ..TrackedAccount::with_options(&name, forks, frozen) };
				// A repo given earlier in the same command is dropped here too, so it isn't synced twice.
				for pin in config.remove_pins_covered(&tracked) {
					println!("{pin} removed (now covered by {}).", tracked.display_name());
					added.retain(|i| !matches!(i, AddedItem::Pin(p) if *p == pin));
					changed = true;
				}
				added.push(AddedItem::Account(tracked.display_name()));
			}
			Resolved::Repo(repo) => {
				let key = repo.full_name;
				if let Some(owner) = config.track.iter().find(|u| u.covers(&key)).map(TrackedAccount::display_name) {
					if let Some(restored) = config.include_repo(&key) {
						println!("Now tracking {restored} again.");
						changed = true;
						let owner = AddedItem::Restored(owner);
						if !added.contains(&owner) {
							added.push(owner);
						}
					} else {
						println!("{owner} is already fully tracked; {key} will be synced automatically.");
					}
				} else if config.is_pinned(&key) {
					println!("Already tracking {key}.");
				} else {
					// A leftover exclusion for an account that's no longer tracked means nothing; drop it.
					config.include_repo(&key);
					config.pin_repo_with_options(&key, repo.id, submodules);
					println!("Now tracking {key}.");
					added.push(AddedItem::Pin(key));
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

/// Records which forge `target`'s host runs the first time something is added from it, and points
/// an `alias:path` target at the host its alias connects to. Returns `true` if the config changed.
async fn register_host(config: &mut Config, target: &mut Target) -> Result<bool> {
	let Some(host) = target.host.clone().filter(|h| !config.hosts.contains_key(h)) else { return Ok(false) };
	if let Some(alias) = target.ssh_alias.clone() {
		let host = forge::ssh_hostname(&alias).await?;
		target.host = Some(host.clone());
		if let Some(entry) = config.hosts.get_mut(&host) {
			// Already added another way; remember the alias so it can name things from now on.
			let new_alias = entry.ssh_alias.is_none() && alias != host;
			if new_alias {
				entry.ssh_alias = Some(alias);
			}
			return Ok(new_alias);
		}
		if !Gitolite::detect(&alias).await {
			bail!("'ssh {alias} info' didn't answer like a gitolite server, so gitkeep can't list {host}'s repos.");
		}
		let ssh_alias = (alias != host).then_some(alias);
		config.hosts.insert(host, HostConfig { kind: ForgeKind::Gitolite, token: None, use_ssh: None, ssh_alias });
		return Ok(true);
	}
	let kind = forge::detect(&host).await?;
	// Scripts get the global setting, since `confirm` answers with its default without a terminal.
	// An answer that matches the global setting isn't stored, so the host keeps following it if it
	// changes.
	let use_ssh =
		Some(confirm(&format!("Use SSH clone URLs for {host}?"), config.use_ssh)?).filter(|&ssh| ssh != config.use_ssh);
	config.hosts.insert(host, HostConfig { kind, token: None, use_ssh, ssh_alias: None });
	Ok(true)
}
