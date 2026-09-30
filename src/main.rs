#![warn(clippy::all, clippy::cargo, clippy::nursery, clippy::pedantic, clippy::absolute_paths)]
#![allow(clippy::multiple_crate_versions)]
#![deny(warnings)]

use anyhow::Result;
use clap::Parser;

mod cli;
mod config;
mod forge;
mod init;
mod login;
mod size;
mod sync;
mod track;
mod utils;

use crate::{
	cli::{Cli, Commands},
	config::Config,
	forge::Target,
};

#[allow(clippy::fn_params_excessive_bools)]
async fn run_add(
	targets: Vec<String>,
	forks: bool,
	frozen: bool,
	submodules: bool,
	no_submodules: bool,
	no_sync: bool,
	sync_flag: bool,
) -> Result<()> {
	let submodules_override = if submodules {
		Some(true)
	} else if no_submodules {
		Some(false)
	} else {
		None
	};
	// --sync and --no-sync override the configured default either way.
	let no_sync = if sync_flag {
		false
	} else if no_sync {
		true
	} else {
		Config::load()?.no_sync
	};
	let added = track::add(&utils::expand_targets(targets)?, forks, frozen, submodules_override).await?;
	if no_sync {
		return Ok(());
	}
	if !added.accounts.is_empty() {
		let opts = sync::SyncOptions {
			force_forks: forks,
			force_submodules: submodules_override.unwrap_or(false),
			..Default::default()
		};
		sync::run_for(&added.accounts, opts).await?;
	}
	if !added.restored_owners.is_empty() {
		sync::run_for(&added.restored_owners, sync::SyncOptions::default()).await?;
	}
	sync::run_pinned(&added.pinned).await
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
	let cli = Cli::parse();
	match cli.command {
		Commands::Init => init::run(),
		Commands::Login => login::run().await,
		Commands::Add { users, forks, frozen, submodules, no_submodules, no_sync, sync } => {
			run_add(users, forks, frozen, submodules, no_submodules, no_sync, sync).await
		}
		Commands::Remove { users, delete, yes } => track::remove(&utils::expand_targets(users)?, delete, yes).await,
		Commands::List => track::list(),
		Commands::Size { format } => size::run(format),
		Commands::Sync { users, forks, submodules, pull_only, new_only, quiet, verbose } => {
			let users: Vec<String> = users.iter().map(|u| Target::parse(u).key()).collect();
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
