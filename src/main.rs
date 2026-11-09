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
	cli::{AddArgs, Cli, Commands},
	config::Config,
};

async fn run_add(args: AddArgs) -> Result<()> {
	let submodules_override = if args.submodules {
		Some(true)
	} else if args.no_submodules {
		Some(false)
	} else {
		None
	};
	let no_sync = if args.sync {
		false
	} else if args.no_sync {
		true
	} else {
		Config::load()?.no_sync
	};
	let targets = utils::expand_targets(args.users)?;
	let added = track::add(&targets, args.forks, args.frozen, args.wikis, submodules_override).await?;
	if no_sync {
		return Ok(());
	}
	sync::run_added(&added).await
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
	let cli = Cli::parse();
	match cli.command {
		Commands::Init => init::run(),
		Commands::Login => login::run().await,
		Commands::Add(args) => run_add(args).await,
		Commands::Remove { users, delete, yes } => track::remove(&utils::expand_targets(users)?, delete, yes).await,
		Commands::Freeze { users } => track::set_frozen(&users, true),
		Commands::Unfreeze { users } => track::set_frozen(&users, false),
		Commands::List => track::list(),
		Commands::Size { format, breakdown } => size::run(format, breakdown),
		Commands::Sync { users, forks, submodules, pull_only, new_only, quiet, verbose, jobs } => {
			let verbosity = if quiet {
				sync::Verbosity::Quiet
			} else if verbose {
				sync::Verbosity::Verbose
			} else {
				sync::Verbosity::Normal
			};
			let opts =
				sync::SyncOptions { force_forks: forks, force_submodules: submodules, pull_only, new_only, jobs };
			sync::run(&utils::expand_targets(users)?, opts, verbosity).await
		}
	}
}
