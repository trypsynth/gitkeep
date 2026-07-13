use clap::{Parser, Subcommand};

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
	/// Add GitHub users, orgs, or individual repos (user/repo) to the archive
	Add {
		#[arg(value_name = "TARGET", required = true)]
		users: Vec<String>,
		/// Include forked repositories from these users
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
	/// Skip a specific repo during sync (use user/repo format)
	Skip {
		#[arg(value_name = "REPO", required = true)]
		repos: Vec<String>,
		/// Also delete the local archive directory for these repos
		#[arg(short, long)]
		delete: bool,
	},
	/// Re-enable a previously skipped repo
	Unskip {
		#[arg(value_name = "REPO", required = true)]
		repos: Vec<String>,
	},
	/// Stop tracking one or more users, orgs, or pinned repos (user/repo)
	#[command(alias = "rm")]
	Remove {
		#[arg(value_name = "TARGET", required = true)]
		users: Vec<String>,
		/// Also delete the local archive directory for these users
		#[arg(short, long)]
		delete: bool,
	},
	/// Delete local copies of all skipped repos
	Prune {
		/// Skip the confirmation prompt
		#[arg(short, long)]
		yes: bool,
	},
	/// Show all tracked users and orgs
	#[command(alias = "ls")]
	List,
	/// Sync all tracked users. Optionally pass usernames to add and sync immediately.
	#[command(alias = "run")]
	Sync {
		/// GitHub usernames or orgs to add to the track list and sync right now
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
