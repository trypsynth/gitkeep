#![warn(clippy::all, clippy::cargo, clippy::nursery, clippy::pedantic)]

use anyhow::Result;
use clap::Parser;

mod cli;
mod config;
mod init;
mod login;
mod sync;
mod track;
mod utils;
mod watch;

use crate::cli::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<()> {
	let cli = Cli::parse();
	match cli.command {
		Commands::Init => init::run(),
		Commands::Login => login::run().await,
		Commands::Add { users, forks, frozen, no_sync } => {
			track::add(&users, forks, frozen)?;
			if !no_sync {
				sync::run_for(&users, forks).await?;
			}
			Ok(())
		}
		Commands::Remove { users, delete } => track::remove(&users, delete),
		Commands::List => track::list(),
		Commands::Run { users, forks, pull_only, new_only } => sync::run(&users, forks, pull_only, new_only).await,
		Commands::Watch { interval } => watch::run(interval).await,
	}
}
