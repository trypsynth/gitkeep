use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "gitkeep", about = "High-performance GitHub archival tool")]
pub struct Cli {
	#[command(subcommand)]
	pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
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
		/// Do not update these accounts in bulk runs after the initial clone
		#[arg(long)]
		frozen: bool,
		/// Add to the tracked list without cloning right now
		#[arg(long)]
		no_sync: bool,
	},
	/// Stop tracking one or more users or orgs
	#[command(alias = "rm")]
	Remove {
		#[arg(value_name = "USERNAME", required = true)]
		users: Vec<String>,
		/// Also delete the local archive directory for these users
		#[arg(short, long)]
		delete: bool,
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
