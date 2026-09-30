use std::{
	io::{self, Write as _},
	path::Path,
	process::{Command, Stdio},
};

use anyhow::{Context, Error, Result, anyhow, bail};

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
		PullOutcome::Failed(anyhow!("git pull failed: {}", failure_reason(&output.stderr, exit_code)))
	}
}

/// Whether a failed `git pull`'s stderr shows the checkout no longer matches the remote (e.g. the
/// repo was deleted and recreated), which needs a re-clone, unlike a network or auth failure.
pub fn indicates_repo_identity_mismatch(stderr: &[u8]) -> bool {
	let stderr = String::from_utf8_lossy(stderr);
	stderr.contains("unrelated histories") || stderr.contains("but no such ref was fetched")
}

/// The most useful part of a failed git command's stderr: its `fatal:`/`error:` lines if it printed
/// any, otherwise its last line. Progress updates are split on `\r` so they don't get in the way.
fn failure_reason(stderr: &[u8], exit_code: i32) -> String {
	let stderr = String::from_utf8_lossy(stderr);
	let lines: Vec<&str> = stderr.split(['\n', '\r']).map(str::trim).filter(|l| !l.is_empty()).collect();
	let errors: Vec<&str> =
		lines.iter().copied().filter(|l| l.starts_with("fatal:") || l.starts_with("error:")).collect();
	if !errors.is_empty() {
		return errors.join(" ");
	}
	lines.last().map_or_else(|| format!("exit code {exit_code}"), |l| (*l).to_string())
}

/// Runs a git subcommand, streaming its output to the terminal in verbose mode. Otherwise output is
/// hidden, but a failure reports what git said. `action` names the command (e.g. `"git clone"`).
pub fn run_git(mut cmd: Command, verbosity: Verbosity, action: &str) -> Result<()> {
	let context = || format!("Could not run '{action}'. Is git installed and on your PATH?");
	if verbosity == Verbosity::Verbose {
		let status = cmd.status().with_context(context)?;
		if !status.success() {
			bail!("{action} failed with exit code {}", status.code().unwrap_or(-1));
		}
		return Ok(());
	}
	let output = cmd.stdout(Stdio::null()).stderr(Stdio::piped()).output().with_context(context)?;
	if !output.status.success() {
		bail!("{action} failed: {}", failure_reason(&output.stderr, output.status.code().unwrap_or(-1)));
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
	fn failure_reason_prefers_fatal_and_error_lines() {
		let stderr =
			b"Cloning into 'x'...\nremote: Enumerating objects: 5\rReceiving objects: 10%\rReceiving objects: 50%\n\
			error: RPC failed; curl 92 HTTP/2 stream 0 was not closed cleanly\nfatal: early EOF\n";
		assert_eq!(
			failure_reason(stderr, 128),
			"error: RPC failed; curl 92 HTTP/2 stream 0 was not closed cleanly fatal: early EOF"
		);
	}

	#[test]
	fn failure_reason_falls_back_to_last_line() {
		assert_eq!(failure_reason(b"Receiving objects: 10%\rsomething went wrong\n", 1), "something went wrong");
	}

	#[test]
	fn failure_reason_uses_exit_code_when_silent() {
		assert_eq!(failure_reason(b"", 128), "exit code 128");
	}

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
