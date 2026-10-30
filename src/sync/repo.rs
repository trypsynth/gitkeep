use std::{
	collections::HashSet,
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
	submodules: bool,
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

/// Every clone and pull a sync needs, gathered across all its accounts and pins so they share one
/// pool of workers. Deciding what each repo needs and recording the results both happen in order,
/// so only the git work itself runs in parallel.
#[derive(Default)]
pub struct Plan {
	jobs: Vec<Job>,
	/// Archive paths already planned, so a repo listed by two tracked accounts (a GitLab group and
	/// one of its subgroups) isn't cloned twice at once.
	planned: HashSet<String>,
}

impl Plan {
	/// Works out what each of `repos` needs and adds it to the plan.
	pub fn add(
		&mut self,
		repos: Vec<RemoteRepo>,
		include_forks: bool,
		use_submodules: bool,
		ctx: SyncContext<'_>,
		config: &Config,
		sync_state: &mut SyncState<'_>,
	) {
		for repo in repos {
			if !self.planned.insert(repo.rel_dir.clone()) {
				continue;
			}
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
			self.jobs.push(Job {
				full_name: repo.full_name,
				id: repo.id,
				pushed_at: repo.pushed_at,
				url,
				repo_dir,
				kind,
				submodules: use_submodules,
			});
		}
	}

	/// Runs every planned job, up to `ctx.jobs` at once, then records the results in plan order.
	pub fn run(self, ctx: SyncContext<'_>, sync_state: &mut SyncState<'_>) {
		let outcomes = run_jobs(&self.jobs, ctx.jobs, ctx.verbosity);
		for (job, outcome) in self.jobs.iter().zip(outcomes) {
			record(job, outcome, ctx.verbosity, sync_state);
		}
	}
}

/// Runs every job on a pool of `workers` threads, returning their outcomes in job order.
fn run_jobs(jobs: &[Job], workers: usize, verbosity: Verbosity) -> Vec<Outcome> {
	let next = AtomicUsize::new(0);
	let outcomes: Vec<OnceLock<Outcome>> = jobs.iter().map(|_| OnceLock::new()).collect();
	thread::scope(|scope| {
		for _ in 0..workers.clamp(1, jobs.len().max(1)) {
			scope.spawn(|| {
				loop {
					let i = next.fetch_add(1, Ordering::Relaxed);
					let Some(job) = jobs.get(i) else { break };
					let _ = outcomes[i].set(run_job(job, verbosity));
				}
			});
		}
	});
	outcomes.into_iter().map(|o| o.into_inner().expect("every job runs")).collect()
}

fn run_job(job: &Job, verbosity: Verbosity) -> Outcome {
	match job.kind {
		JobKind::Clone => {
			if verbosity == Verbosity::Verbose {
				println!("Cloning {}...", job.full_name);
			}
			clone(job, verbosity, "clone")
		}
		JobKind::Reclone => reclone(job, verbosity),
		JobKind::Pull => {
			if verbosity == Verbosity::Verbose {
				println!("Pulling {}...", job.full_name);
			}
			match git_pull(&job.repo_dir, verbosity) {
				PullOutcome::Updated => Outcome::Updated { warning: submodules(job, verbosity, "update") },
				PullOutcome::UpToDate => Outcome::UpToDate,
				PullOutcome::Fatal => {
					if verbosity == Verbosity::Verbose {
						println!("  Pull failed for {}, re-cloning...", job.full_name);
					}
					reclone(job, verbosity)
				}
				PullOutcome::Failed(e) => Outcome::Failed(format!("Failed to pull {}: {e:#}.", job.full_name)),
			}
		}
	}
}

/// Removes an existing local clone and clones it fresh, e.g. when the remote history is
/// gone (exit 128) or `owner/name` now points at an unrelated repo (stored id changed).
fn reclone(job: &Job, verbosity: Verbosity) -> Outcome {
	if let Err(e) = fs::remove_dir_all(&job.repo_dir) {
		return Outcome::Failed(format!("Could not remove {}: {e}.", job.repo_dir.display()));
	}
	clone(job, verbosity, "re-clone")
}

fn clone(job: &Job, verbosity: Verbosity, action: &str) -> Outcome {
	match git_clone(&job.url, &job.repo_dir, verbosity) {
		Ok(()) => Outcome::Cloned { warning: submodules(job, verbosity, "clone") },
		Err(e) => Outcome::Failed(format!("Failed to {action} {}: {e:#}.", job.full_name)),
	}
}

/// Updates the job's submodules if they're wanted, returning a warning if that fails.
fn submodules(job: &Job, verbosity: Verbosity, verb: &str) -> Option<String> {
	if !job.submodules {
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

	fn remote(name: &str, ssh_url: Option<&str>) -> RemoteRepo {
		RemoteRepo {
			full_name: name.into(),
			rel_dir: name.into(),
			id: 1,
			pushed_at: None,
			fork: false,
			clone_url: Some(format!("https://example.com/{name}.git")),
			ssh_url: ssh_url.map(Into::into),
		}
	}

	/// Plans `repos` (as if listed by separate accounts) against an empty archive and returns the
	/// planned jobs with the totals planning recorded.
	fn plan(config: &Config, lists: Vec<Vec<RemoteRepo>>) -> (Vec<Job>, Totals) {
		let root = env::temp_dir().join(format!("gitkeep-plan-test-{}", process::id()));
		let ctx =
			SyncContext { archive_dir: &root, opts: SyncOptions::default(), verbosity: Verbosity::Quiet, jobs: 1 };
		let mut state = State::default();
		let mut totals = Totals::default();
		let mut sync_state = SyncState { state: &mut state, totals: &mut totals };
		let mut plan = Plan::default();
		for repos in lists {
			plan.add(repos, false, false, ctx, config, &mut sync_state);
		}
		let _ = fs::remove_dir_all(&root);
		(plan.jobs, totals)
	}

	#[test]
	fn plan_counts_a_repo_without_the_chosen_clone_url_as_failed() {
		let mut config = Config::default();
		config.use_ssh = true;
		let (jobs, totals) = plan(&config, vec![vec![remote("alice/repo", None)]]);
		assert!(jobs.is_empty());
		assert_eq!((totals.failed, totals.excluded), (1, 0));
	}

	#[test]
	fn plan_skips_a_repo_already_planned_by_another_account() {
		let config = Config::default();
		let (jobs, totals) =
			plan(&config, vec![vec![remote("h.com/grp/sub/p", None)], vec![remote("h.com/grp/sub/p", None)]]);
		assert_eq!(jobs.len(), 1);
		assert_eq!((totals.failed, totals.excluded), (0, 0));
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
			submodules: false,
		};
		let source_url = source.to_string_lossy();
		let missing_url = root.join("missing").to_string_lossy().into_owned();
		let jobs: Vec<Job> = (0..6).map(|i| if i == 3 { job(i, &missing_url) } else { job(i, &source_url) }).collect();
		let outcomes = run_jobs(&jobs, 4, Verbosity::Quiet);
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
