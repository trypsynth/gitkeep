use clap::{Parser, Subcommand};

use crate::size::SizeFormat;

#[derive(Parser)]
#[command(name = "gitkeep", about = "High-performance GitHub archival tool")]
pub struct Cli {
	#[command(subcommand)]
	pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
	/// Configure the archive directory and clone URL settings
	Init,
	/// Authenticate with a GitHub personal access token
	Login,
	/// Add accounts or single repos: GitHub names (owner, owner/repo), host/path, or URLs
	Add {
		#[arg(value_name = "TARGET", required = true)]
		users: Vec<String>,
		/// Include forked repositories from these accounts
		#[arg(long)]
		forks: bool,
		/// Do not update these accounts in bulk runs after the initial clone
		#[arg(long)]
		frozen: bool,
		/// Clone submodules for these accounts/repos, overriding the global default
		#[arg(long, conflicts_with = "no_submodules")]
		submodules: bool,
		/// Never clone submodules for these accounts/repos, overriding the global default
		#[arg(long, conflicts_with = "submodules")]
		no_submodules: bool,
		/// Add to the tracked list without cloning right now, overriding the config default
		#[arg(long, conflicts_with = "sync")]
		no_sync: bool,
		/// Clone immediately after adding, overriding a `no_sync = true` config default
		#[arg(long, conflicts_with = "no_sync")]
		sync: bool,
	},
	/// Stop tracking accounts or repos, including single repos of a tracked account
	#[command(alias = "rm")]
	Remove {
		#[arg(value_name = "TARGET", required = true)]
		users: Vec<String>,
		/// Also delete the local archive directory for these targets
		#[arg(short, long)]
		delete: bool,
		/// Skip all confirmation prompts, assuming "yes"
		#[arg(short, long)]
		yes: bool,
	},
	/// Show all tracked accounts and repos
	#[command(alias = "ls")]
	List,
	/// Show the on-disk size of the archive, broken down per account
	#[command(alias = "du")]
	Size {
		/// Size unit format to display
		#[arg(short = 's', long, value_enum, default_value = "binary")]
		format: SizeFormat,
	},
	/// Sync every tracked account that isn't frozen, plus individually tracked repos
	#[command(alias = "run")]
	Sync {
		/// GitHub accounts to start tracking before syncing
		#[arg(value_name = "USERNAME")]
		users: Vec<String>,
		/// Include forked repositories for this sync only (does not save to config)
		#[arg(long)]
		forks: bool,
		/// Clone submodules for this sync only (does not save to config)
		#[arg(long)]
		submodules: bool,
		/// Only pull existing repos; skip checking for new ones
		#[arg(short = 'p', long)]
		pull_only: bool,
		/// Only check for and clone new repos; skip pulling existing ones
		#[arg(short = 'n', long)]
		new_only: bool,
		/// Suppress all output except errors and the final summary
		#[arg(short = 'q', long)]
		quiet: bool,
		/// Show raw git output
		#[arg(short = 'v', long)]
		verbose: bool,
	},
}
