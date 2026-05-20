#![warn(clippy::all, clippy::cargo, clippy::nursery, clippy::pedantic)]
#![allow(clippy::multiple_crate_versions)]

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
		Commands::Add { users, forks, frozen, no_sync } => {
			let config = crate::config::Config::load()?;
			let client = config.build_client()?;
			let mut resolved = Vec::with_capacity(users.len());
			for name in &users {
				let canonical = sync::resolve_login(&client, name).await?;
				if canonical != *name {
					println!("Resolved '{name}' to '{canonical}'.");
				}
				resolved.push(canonical);
			}
			track::add(&resolved, forks, frozen)?;
			if !no_sync {
				sync::run_for(&resolved, forks).await?;
			}
			Ok(())
		}
		Commands::Skip { repos } => skip::add(&repos).await,
		Commands::Unskip { repos } => skip::remove(&repos),
		Commands::Remove { users, delete } => track::remove(&users, delete),
		Commands::List => track::list(),
		Commands::Run { users, forks, pull_only, new_only, quiet, verbose } => {
			let verbosity = if quiet {
				sync::Verbosity::Quiet
			} else if verbose {
				sync::Verbosity::Verbose
			} else {
				sync::Verbosity::Normal
			};
			sync::run(&users, forks, pull_only, new_only, verbosity).await
		}
	}
}
