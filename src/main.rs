use anyhow::Result;
use clap::{Parser, Subcommand};

mod config;
mod init;
mod login;
mod sync;
mod track;
mod watch;

#[derive(Parser)]
#[command(name = "gitkeep", about = "High-performance GitHub archival tool")]
struct Cli {
	#[command(subcommand)]
	command: Commands,
}

#[derive(Subcommand)]
enum Commands {
	/// Create or reset the config file interactively
	Init,
	/// Authenticate with a GitHub personal access token
	Login,
	/// Add one or more GitHub users or orgs to the archive list
	Add {
		#[arg(value_name = "USERNAME", required = true)]
		users: Vec<String>,
		/// Include forked repositories from these users
		#[arg(long)]
		forks: bool,
	},
	/// Stop tracking one or more users or orgs
	#[command(alias = "rm")]
	Remove {
		#[arg(value_name = "USERNAME", required = true)]
		users: Vec<String>,
	},
	/// Show all tracked users and orgs
	#[command(alias = "ls")]
	List,
	/// Sync all tracked users. Optionally pass usernames to add and sync immediately.
	#[command(alias = "sync")]
	Run {
		/// GitHub usernames or orgs to add to the track list and sync right now
		#[arg(value_name = "USERNAME")]
		users: Vec<String>,
		/// Include forked repositories for this sync only (does not save to config)
		#[arg(long)]
		forks: bool,
	},
	/// Daemon mode: sync all tracked users on a schedule
	Watch {
		/// Seconds between syncs
		#[arg(short, long, default_value_t = 3600)]
		interval: u64,
	},
}

#[tokio::main]
async fn main() -> Result<()> {
	let cli = Cli::parse();
	match cli.command {
		Commands::Init => init::run(),
		Commands::Login => login::run().await,
		Commands::Add { users, forks } => track::add(&users, forks),
		Commands::Remove { users } => track::remove(&users),
		Commands::List => track::list(),
		Commands::Run { users, forks } => sync::run(&users, forks).await,
		Commands::Watch { interval } => watch::run(interval).await,
	}
}
