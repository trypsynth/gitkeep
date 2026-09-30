use std::{
	io::{self, Write as _},
	path::Path,
	process::{Command, Stdio},
};

use anyhow::{Context, Error, Result, bail};

use super::Verbosity;

pub fn git_head(repo_dir: &Path) -> Option<String> {
	let out = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(repo_dir).output().ok()?;
	if out.status.success() { Some(String::from_utf8_lossy(&out.stdout).trim().to_string()) } else { None }
}

pub enum PullOutcome {
	Updated,
	UpToDate,
	Fatal,
	Failed(Error),
}

pub fn git_pull(repo_dir: &Path, verbosity: Verbosity) -> PullOutcome {
	let head_before = git_head(repo_dir);
	let output = match Command::new("git").arg("pull").current_dir(repo_dir).output() {
		Ok(out) => out,
		Err(e) => {
			return PullOutcome::Failed(
				Error::from(e).context("Could not run 'git pull'. Is git installed and on your PATH?"),
			);
		}
	};
	if verbosity == Verbosity::Verbose {
		io::stdout().write_all(&output.stdout).ok();
		io::stderr().write_all(&output.stderr).ok();
	}
	let exit_code = output.status.code().unwrap_or(-1);
	if exit_code == 0 {
		let head_after = git_head(repo_dir);
		if head_before == head_after { PullOutcome::UpToDate } else { PullOutcome::Updated }
	} else if exit_code == 128 || indicates_repo_identity_mismatch(&output.stderr) {
		PullOutcome::Fatal
	} else {
		PullOutcome::Failed(anyhow::anyhow!("git pull failed with code {exit_code}."))
	}
}

/// Whether a failed `git pull`'s stderr shows the checkout no longer matches the remote (e.g. the
/// repo was deleted and recreated), which needs a re-clone, unlike a network or auth failure.
pub fn indicates_repo_identity_mismatch(stderr: &[u8]) -> bool {
	let stderr = String::from_utf8_lossy(stderr);
	stderr.contains("unrelated histories") || stderr.contains("but no such ref was fetched")
}

/// Runs a git subcommand, streaming its output to the terminal in verbose mode and suppressing
/// it otherwise. `action` names the command in error messages (e.g. `"git clone"`).
pub fn run_git(mut cmd: Command, verbosity: Verbosity, action: &str) -> Result<()> {
	let status = if verbosity == Verbosity::Verbose {
		cmd.status()
	} else {
		cmd.stdout(Stdio::null()).stderr(Stdio::null()).status()
	}
	.with_context(|| format!("Could not run '{action}'. Is git installed and on your PATH?"))?;
	if !status.success() {
		bail!("{action} failed with code {}.", status.code().unwrap_or(-1));
	}
	Ok(())
}

pub fn git_clone(url: &str, dest: &Path, verbosity: Verbosity) -> Result<()> {
	let mut cmd = Command::new("git");
	cmd.args(["clone", "--", url]).arg(dest);
	run_git(cmd, verbosity, "git clone")
}

/// Initializes and updates submodules to the commit recorded by the superproject. Idempotent,
/// and a no-op if the repo has no `.gitmodules`, so it's safe to call after every clone/pull.
pub fn update_submodules(repo_dir: &Path, verbosity: Verbosity) -> Result<()> {
	if !repo_dir.join(".gitmodules").exists() {
		return Ok(());
	}
	let mut cmd = Command::new("git");
	cmd.args(["submodule", "update", "--init", "--recursive"]).current_dir(repo_dir);
	run_git(cmd, verbosity, "git submodule update")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn identity_mismatch_detects_unrelated_histories() {
		assert!(indicates_repo_identity_mismatch(b"fatal: refusing to merge unrelated histories"));
	}

	#[test]
	fn identity_mismatch_detects_missing_tracked_ref() {
		let stderr = b"Your configuration specifies to merge with the ref 'refs/heads/master'\n\
			from the remote, but no such ref was fetched.";
		assert!(indicates_repo_identity_mismatch(stderr));
	}

	#[test]
	fn identity_mismatch_false_for_transient_failure() {
		assert!(!indicates_repo_identity_mismatch(
			b"fatal: unable to access 'https://github.com/x/y.git': Could not resolve host"
		));
	}
}
