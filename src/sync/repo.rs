use std::{
	fs,
	path::PathBuf,
	sync::{
		OnceLock,
		atomic::{AtomicUsize, Ordering},
	},
	thread,
};

use chrono::{DateTime, Utc};

use super::{
	SyncContext, SyncState, Verbosity,
	git::{PullOutcome, git_clone, git_pull, update_submodules},
};
use crate::{config::Config, forge::RemoteRepo};

/// A clone or pull that a repo needs, decided before any git command runs.
struct Job {
	full_name: String,
	id: u64,
	pushed_at: Option<DateTime<Utc>>,
	url: String,
	repo_dir: PathBuf,
	kind: JobKind,
}

enum JobKind {
	Clone,
	Pull,
	/// The local checkout belongs to a different repo now, so start over.
	Reclone,
}

/// What running a job did. A submodule failure doesn't fail the repo, so it rides along as a warning.
enum Outcome {
	Cloned { warning: Option<String> },
	Updated { warning: Option<String> },
	UpToDate,
	Failed(String),
}

/// Clones or pulls every repo in `repos` that needs it, running up to `ctx.jobs` git commands at
/// once. Deciding what each repo needs and recording the results both happen in order, so only the
/// git work itself runs in parallel.
pub fn sync_repo_list(
	repos: Vec<RemoteRepo>,
	include_forks: bool,
	use_submodules: bool,
	ctx: SyncContext<'_>,
	config: &Config,
	sync_state: &mut SyncState<'_>,
) {
	let jobs = plan_jobs(repos, include_forks, ctx, config, sync_state);
	let outcomes = run_jobs(&jobs, ctx.jobs, use_submodules, ctx.verbosity);
	for (job, outcome) in jobs.iter().zip(outcomes) {
		record(job, outcome, ctx.verbosity, sync_state);
	}
}

fn plan_jobs(
	repos: Vec<RemoteRepo>,
	include_forks: bool,
	ctx: SyncContext<'_>,
	config: &Config,
	sync_state: &mut SyncState<'_>,
) -> Vec<Job> {
	let mut jobs = Vec::new();
	for repo in repos {
		let full_name = repo.full_name.as_str();
		if config.is_excluded(full_name) || (repo.fork && !include_forks) {
			sync_state.totals.excluded += 1;
			continue;
		}
		let use_ssh = config.use_ssh_for(full_name);
		let Some(url) = repo.clone_url(use_ssh) else {
			let (missing, other) = if use_ssh { ("SSH", "HTTPS") } else { ("HTTPS", "SSH") };
			eprintln!(
				"  Could not sync {full_name}: the forge gave no {missing} clone URL for it. Setting use_ssh in the config to use {other} instead may work."
			);
			sync_state.totals.failed += 1;
			continue;
		};
		let mut repo_dir = ctx.archive_dir.to_path_buf();
		repo_dir.extend(repo.rel_dir.split('/'));
		let already_cloned = repo_dir.exists();
		if (already_cloned && ctx.opts.new_only) || (!already_cloned && ctx.opts.pull_only) {
			sync_state.totals.excluded += 1;
			continue;
		}
		let kind = if already_cloned {
			let stored = sync_state.state.repos.get(full_name);
			if stored.and_then(|s| s.id).is_some_and(|stored_id| stored_id != repo.id) {
				if ctx.verbosity != Verbosity::Quiet {
					println!("  {full_name} was recreated as a different repository, re-cloning...");
				}
				JobKind::Reclone
			} else if should_skip_pull(repo.pushed_at, stored.and_then(|s| s.pushed_at)) {
				sync_state.totals.pulled_up_to_date += 1;
				continue;
			} else {
				JobKind::Pull
			}
		} else {
			if let Some(parent) = repo_dir.parent()
				&& let Err(e) = fs::create_dir_all(parent)
			{
				eprintln!("  Could not create directory for {full_name}: {e}.");
				sync_state.totals.failed += 1;
				continue;
			}
			JobKind::Clone
		};
		jobs.push(Job { full_name: repo.full_name, id: repo.id, pushed_at: repo.pushed_at, url, repo_dir, kind });
	}
	jobs
}

/// Runs every job on a pool of `workers` threads, returning their outcomes in job order.
fn run_jobs(jobs: &[Job], workers: usize, use_submodules: bool, verbosity: Verbosity) -> Vec<Outcome> {
	let next = AtomicUsize::new(0);
	let outcomes: Vec<OnceLock<Outcome>> = jobs.iter().map(|_| OnceLock::new()).collect();
	thread::scope(|scope| {
		for _ in 0..workers.clamp(1, jobs.len().max(1)) {
			scope.spawn(|| {
				loop {
					let i = next.fetch_add(1, Ordering::Relaxed);
					let Some(job) = jobs.get(i) else { break };
					let _ = outcomes[i].set(run_job(job, use_submodules, verbosity));
				}
			});
		}
	});
	outcomes.into_iter().map(|o| o.into_inner().expect("every job runs")).collect()
}
fn run_job(job: &Job, use_submodules: bool, verbosity: Verbosity) -> Outcome {
	match job.kind {
		JobKind::Clone => {
			if verbosity == Verbosity::Verbose {
				println!("Cloning {}...", job.full_name);
			}
			clone(job, use_submodules, verbosity, "clone")
		}
		JobKind::Reclone => reclone(job, use_submodules, verbosity),
		JobKind::Pull => {
			if verbosity == Verbosity::Verbose {
				println!("Pulling {}...", job.full_name);
			}
			match git_pull(&job.repo_dir, verbosity) {
				PullOutcome::Updated => {
					Outcome::Updated { warning: submodules(job, use_submodules, verbosity, "update") }
				}
				PullOutcome::UpToDate => Outcome::UpToDate,
				PullOutcome::Fatal => {
					if verbosity == Verbosity::Verbose {
						println!("  Pull failed for {}, re-cloning...", job.full_name);
					}
					reclone(job, use_submodules, verbosity)
				}
				PullOutcome::Failed(e) => Outcome::Failed(format!("Failed to pull {}: {e:#}.", job.full_name)),
			}
		}
	}
}

/// Removes an existing local clone and clones it fresh, e.g. when the remote history is
/// gone (exit 128) or `owner/name` now points at an unrelated repo (stored id changed).
fn reclone(job: &Job, use_submodules: bool, verbosity: Verbosity) -> Outcome {
	if let Err(e) = fs::remove_dir_all(&job.repo_dir) {
		return Outcome::Failed(format!("Could not remove {}: {e}.", job.repo_dir.display()));
	}
	clone(job, use_submodules, verbosity, "re-clone")
}

fn clone(job: &Job, use_submodules: bool, verbosity: Verbosity, action: &str) -> Outcome {
	match git_clone(&job.url, &job.repo_dir, verbosity) {
		Ok(()) => Outcome::Cloned { warning: submodules(job, use_submodules, verbosity, "clone") },
		Err(e) => Outcome::Failed(format!("Failed to {action} {}: {e:#}.", job.full_name)),
	}
}

/// Updates the job's submodules if they're wanted, returning a warning if that fails.
fn submodules(job: &Job, use_submodules: bool, verbosity: Verbosity, verb: &str) -> Option<String> {
	if !use_submodules {
		return None;
	}
	update_submodules(&job.repo_dir, verbosity)
		.err()
		.map(|e| format!("Could not {verb} submodules for {}: {e:#}.", job.full_name))
}

fn record(job: &Job, outcome: Outcome, verbosity: Verbosity, sync_state: &mut SyncState<'_>) {
	let state = &mut *sync_state.state;
	let totals = &mut *sync_state.totals;
	match outcome {
		Outcome::Cloned { warning } => {
			state.mark_synced(&job.full_name, job.pushed_at, job.id);
			warn(warning);
			if verbosity == Verbosity::Normal {
				totals.new_repos.push(job.full_name.clone());
			}
			totals.cloned += 1;
		}
		Outcome::Updated { warning } => {
			state.mark_synced(&job.full_name, job.pushed_at, job.id);
			warn(warning);
			if verbosity == Verbosity::Normal {
				totals.updated_repos.push(job.full_name.clone());
			}
			totals.pulled_updated += 1;
		}
		Outcome::UpToDate => {
			state.mark_synced(&job.full_name, job.pushed_at, job.id);
			totals.pulled_up_to_date += 1;
		}
		Outcome::Failed(message) => {
			eprintln!("  {message}");
			totals.failed += 1;
		}
	}
}

fn warn(warning: Option<String>) {
	if let Some(warning) = warning {
		eprintln!("  {warning}");
	}
}

fn should_skip_pull(repo_pushed_at: Option<DateTime<Utc>>, state_pushed_at: Option<DateTime<Utc>>) -> bool {
	match (repo_pushed_at, state_pushed_at) {
		(Some(repo), Some(state)) => repo <= state,
		_ => false,
	}
}

#[cfg(test)]
mod tests {
	use std::{env, process, process::Command};

	use chrono::Duration;

	use super::{super::SyncOptions, *};
	use crate::{config::State, sync::summary::Totals};

	#[test]
	fn plan_jobs_counts_a_repo_without_the_chosen_clone_url_as_failed() {
		let mut config = Config::default();
		config.use_ssh = true;
		let repo = RemoteRepo {
			full_name: "alice/repo".into(),
			rel_dir: "alice/repo".into(),
			id: 1,
			pushed_at: None,
			fork: false,
			clone_url: Some("https://example.com/alice/repo.git".into()),
			ssh_url: None,
		};
		let root = env::temp_dir().join(format!("gitkeep-plan-test-{}", process::id()));
		let ctx =
			SyncContext { archive_dir: &root, opts: SyncOptions::default(), verbosity: Verbosity::Quiet, jobs: 1 };
		let mut state = State::default();
		let mut totals = Totals::default();
		let mut sync_state = SyncState { state: &mut state, totals: &mut totals };
		assert!(plan_jobs(vec![repo], false, ctx, &config, &mut sync_state).is_empty());
		assert_eq!((totals.failed, totals.excluded), (1, 0));
	}

	#[test]
	fn run_jobs_returns_outcomes_in_job_order_across_workers() {
		let root = env::temp_dir().join(format!("gitkeep-jobs-test-{}", process::id()));
		let source = root.join("source");
		fs::create_dir_all(&source).unwrap();
		let git = |args: &[&str]| {
			assert!(Command::new("git").args(args).current_dir(&source).output().unwrap().status.success());
		};
		git(&["init", "-q"]);
		git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "init"]);
		let job = |i: usize, url: &str| Job {
			full_name: format!("owner/repo{i}"),
			id: 0,
			pushed_at: None,
			url: url.to_string(),
			repo_dir: root.join(format!("repo{i}")),
			kind: JobKind::Clone,
		};
		let source_url = source.to_string_lossy();
		let missing_url = root.join("missing").to_string_lossy().into_owned();
		let jobs: Vec<Job> = (0..6).map(|i| if i == 3 { job(i, &missing_url) } else { job(i, &source_url) }).collect();
		let outcomes = run_jobs(&jobs, 4, false, Verbosity::Quiet);
		fs::remove_dir_all(&root).unwrap();
		for (i, outcome) in outcomes.iter().enumerate() {
			match outcome {
				Outcome::Failed(message) => {
					assert_eq!(i, 3);
					assert!(message.contains("owner/repo3"), "got: {message}");
				}
				Outcome::Cloned { warning: None } => assert_ne!(i, 3),
				_ => panic!("unexpected outcome for job {i}"),
			}
		}
	}

	#[test]
	fn skip_pull_when_pushed_at_matches() {
		let t = Utc::now();
		assert!(should_skip_pull(Some(t), Some(t)));
	}

	#[test]
	fn skip_pull_when_repo_older_than_state() {
		let older = Utc::now() - Duration::hours(1);
		let newer = Utc::now();
		assert!(should_skip_pull(Some(older), Some(newer)));
	}

	#[test]
	fn pull_when_repo_pushed_at_is_newer() {
		let older = Utc::now() - Duration::hours(1);
		let newer = Utc::now();
		assert!(!should_skip_pull(Some(newer), Some(older)));
	}

	#[test]
	fn pull_when_no_state_pushed_at() {
		assert!(!should_skip_pull(Some(Utc::now()), None));
	}

	#[test]
	fn pull_when_no_repo_pushed_at() {
		assert!(!should_skip_pull(None, Some(Utc::now())));
	}

	#[test]
	fn pull_when_neither_pushed_at() {
		assert!(!should_skip_pull(None, None));
	}
}
