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
		Commands::Add { users, forks, no_sync } => {
			track::add(&users, forks)?;
			if !no_sync {
				sync::run_for(&users, forks).await?;
			}
			Ok(())
		}
		Commands::Remove { users } => track::remove(&users),
		Commands::List => track::list(),
		Commands::Run { users, forks } => sync::run(&users, forks).await,
		Commands::Watch { interval } => watch::run(interval).await,
	}
}
