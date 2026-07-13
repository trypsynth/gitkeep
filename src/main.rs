#![warn(clippy::all, clippy::cargo, clippy::nursery, clippy::pedantic)]
#![allow(clippy::multiple_crate_versions)]
#![deny(warnings)]

use anyhow::Result;
use clap::Parser;

mod cli;
mod config;
mod init;
mod login;
mod skip;
mod sync;
mod track;
mod utils;

use crate::cli::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<()> {
	let cli = Cli::parse();
	match cli.command {
		Commands::Init => init::run(),
		Commands::Login => login::run().await,
		Commands::Add { users, forks, frozen, submodules, no_submodules, no_sync, sync } => {
			let (repos, usernames): (Vec<String>, Vec<String>) = users.into_iter().partition(|s| s.contains('/'));
			let submodules_override = if submodules {
				Some(true)
			} else if no_submodules {
				Some(false)
			} else {
				None
			};
			let config = crate::config::Config::load()?;
			// --sync / --no-sync override the configured default in either direction;
			// with neither flag passed, fall back to the config's `no_sync` default.
			let no_sync = if sync { false } else if no_sync { true } else { config.no_sync };

			// Handle plain usernames first so that if someone mixes both formats
			// (e.g. `gitkeep add rust-lang rust-lang/mdBook`), the full-user tracking
			// wins and the individual pin is skipped cleanly.
			if !usernames.is_empty() {
				let client = config.build_client()?;
				let mut resolved = Vec::with_capacity(usernames.len());
				for name in &usernames {
					let canonical = sync::resolve_login(&client, name).await?;
					if canonical != *name {
						println!("Resolved '{name}' to '{canonical}'.");
					}
					resolved.push(canonical);
				}
				track::add(&resolved, forks, frozen, submodules_override)?;
				if !no_sync {
					let opts = sync::SyncOptions {
						force_forks: forks,
						force_submodules: submodules_override.unwrap_or(false),
						..Default::default()
					};
					sync::run_for(&resolved, opts).await?;
				}
			}

			// Handle individual repo pins.
			if !repos.is_empty() {
				let client = config.build_client()?;
				let newly_pinned = track::add_pinned(&repos, &client, submodules_override).await?;
				if !no_sync {
					sync::run_pinned(&newly_pinned).await?;
				}
			}

			Ok(())
		}
		Commands::Skip { repos, delete } => skip::add(&repos, delete).await,
		Commands::Prune { yes } => skip::prune(yes),
		Commands::Unskip { repos } => skip::remove(&repos),
		Commands::Remove { users, delete } => track::remove(&users, delete),
		Commands::List => track::list(),
		Commands::Sync { users, forks, submodules, pull_only, new_only, quiet, verbose } => {
			let verbosity = if quiet {
				sync::Verbosity::Quiet
			} else if verbose {
				sync::Verbosity::Verbose
			} else {
				sync::Verbosity::Normal
			};
			let opts = sync::SyncOptions { force_forks: forks, force_submodules: submodules, pull_only, new_only };
			sync::run(&users, opts, verbosity).await
		}
	}
}
