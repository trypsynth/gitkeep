use std::{
	fs::{self, File},
	io::{self, IsTerminal as _, Write},
	path::Path,
};

use anyhow::{Context, Error, Result, bail};
use crossterm::{
	event::{self, Event, KeyCode, KeyEventKind},
	terminal::{disable_raw_mode, enable_raw_mode},
};

pub fn plural(n: usize, singular: &str, plural: &str) -> String {
	if n == 1 { format!("1 {singular}") } else { format!("{n} {plural}") }
}

/// Writes `contents` to `path` through a temporary file beside it that's renamed into place, so a
/// crash or a full disk leaves the old file whole instead of truncated. A symlinked `path` is
/// written through to its target, and the old file's permissions are kept.
pub fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
	let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
	let mut tmp_name = path.file_name().unwrap_or_default().to_owned();
	tmp_name.push(".tmp");
	let tmp = path.with_file_name(tmp_name);
	let write = || {
		let mut file = File::create(&tmp)?;
		if let Ok(meta) = fs::metadata(&path) {
			file.set_permissions(meta.permissions())?;
		}
		file.write_all(contents.as_bytes())?;
		file.sync_all()?;
		fs::rename(&tmp, &path)
	};
	write().inspect_err(|_| {
		let _ = fs::remove_file(&tmp);
	})
}

/// Expands `owner/a,b,c` into one `owner/name` target per repo (repo names can't contain commas).
/// The list follows the last `/`, so `host/group/a,b` works for GitLab too. Other targets pass
/// through unchanged.
pub fn expand_targets(args: Vec<String>) -> Result<Vec<String>> {
	let mut out = Vec::with_capacity(args.len());
	for target in args {
		let Some((owner, list)) = target.rsplit_once('/').filter(|(_, rest)| rest.contains(',')) else {
			out.push(target);
			continue;
		};
		let names: Vec<&str> = list.split(',').map(str::trim).filter(|n| !n.is_empty()).collect();
		if names.is_empty() {
			bail!("'{target}' doesn't list any repos");
		}
		out.extend(names.into_iter().map(|name| format!("{owner}/{name}")));
	}
	Ok(out)
}

/// Asks for a line of text. An empty answer gives `default`, which is shown in brackets.
pub fn ask(message: &str, default: Option<&str>) -> Result<String> {
	match default {
		Some(default) => print!("{message} [{default}]: "),
		None => print!("{message}: "),
	}
	io::stdout().flush().context("Could not flush stdout")?;
	let mut line = String::new();
	if io::stdin().read_line(&mut line).context("Could not read input")? == 0 {
		bail!("No input");
	}
	let answer = line.trim();
	Ok(if answer.is_empty() { default.unwrap_or_default() } else { answer }.to_string())
}

/// A single-key confirmation prompt that returns immediately on 'y', 'n', or Enter. Without a
/// terminal to read a key from, as in a script, it answers with `default`.
pub fn confirm(message: &str, default: bool) -> Result<bool> {
	let hint = if default { "[Y/n]" } else { "[y/N]" };
	print!("{message} {hint} ");
	if !io::stdin().is_terminal() {
		println!("{}", if default { "Yes" } else { "No" });
		return Ok(default);
	}
	io::stdout().flush().context("Could not flush stdout")?;
	enable_raw_mode().context("Could not enable raw mode")?;
	let result = loop {
		match event::read() {
			Ok(Event::Key(key)) => {
				if key.kind == KeyEventKind::Release {
					continue;
				}
				match key.code {
					KeyCode::Char('y' | 'Y') => break Ok(true),
					KeyCode::Char('n' | 'N') | KeyCode::Esc => break Ok(false),
					KeyCode::Enter => break Ok(default),
					KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
						break Err(anyhow::anyhow!("Interrupted by user"));
					}
					_ => {}
				}
			}
			Ok(_) => {}
			Err(e) => break Err(Error::from(e)),
		}
	};
	disable_raw_mode().ok();
	match result {
		Ok(val) => {
			println!("{}", if val { "Yes" } else { "No" });
			Ok(val)
		}
		Err(e) => {
			println!();
			Err(e)
		}
	}
}

#[cfg(test)]
mod tests {
	use std::{env, path::PathBuf, process};

	use super::*;

	fn args(list: &[&str]) -> Vec<String> {
		list.iter().map(ToString::to_string).collect()
	}

	#[test]
	fn expand_targets_passes_through_plain_targets() {
		let input = args(&["alice", "bob/repo"]);
		assert_eq!(expand_targets(input.clone()).unwrap(), input);
	}

	#[test]
	fn expand_targets_expands_comma_list() {
		assert_eq!(expand_targets(args(&["daisy/a,b,c"])).unwrap(), args(&["daisy/a", "daisy/b", "daisy/c"]));
	}

	#[test]
	fn expand_targets_keeps_nested_owner_path() {
		assert_eq!(
			expand_targets(args(&["gitlab.example.com/grp/sub/a,b"])).unwrap(),
			args(&["gitlab.example.com/grp/sub/a", "gitlab.example.com/grp/sub/b"])
		);
	}

	#[test]
	fn expand_targets_ignores_empty_entries() {
		assert_eq!(expand_targets(args(&["daisy/a,,b,"])).unwrap(), args(&["daisy/a", "daisy/b"]));
	}

	#[test]
	fn expand_targets_rejects_empty_list() {
		assert!(expand_targets(args(&["daisy/,"])).is_err());
	}

	fn temp_dir(name: &str) -> PathBuf {
		let dir = env::temp_dir().join(format!("gitkeep-utils-{name}-{}", process::id()));
		let _ = fs::remove_dir_all(&dir);
		fs::create_dir_all(&dir).unwrap();
		dir
	}

	#[test]
	fn write_atomic_replaces_contents_and_leaves_no_temp_file() {
		let dir = temp_dir("replace");
		let path = dir.join("config.toml");
		fs::write(&path, "old old old").unwrap();
		write_atomic(&path, "new").unwrap();
		assert_eq!(fs::read_to_string(&path).unwrap(), "new");
		assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
		fs::remove_dir_all(&dir).unwrap();
	}

	#[cfg(unix)]
	#[test]
	fn write_atomic_keeps_permissions_and_symlinks() {
		use std::os::unix::fs::{PermissionsExt as _, symlink};

		let dir = temp_dir("unix");
		let target = dir.join("real.toml");
		let link = dir.join("link.toml");
		fs::write(&target, "old").unwrap();
		fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
		symlink(&target, &link).unwrap();
		write_atomic(&link, "new").unwrap();
		assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
		assert_eq!(fs::read_to_string(&target).unwrap(), "new");
		assert_eq!(fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o600);
		fs::remove_dir_all(&dir).unwrap();
	}

	#[test]
	fn plural_uses_singular_for_one() {
		assert_eq!(plural(1, "repo", "repos"), "1 repo");
	}

	#[test]
	fn plural_uses_plural_for_zero() {
		assert_eq!(plural(0, "repo", "repos"), "0 repos");
	}

	#[test]
	fn plural_uses_plural_for_many() {
		assert_eq!(plural(5, "repo", "repos"), "5 repos");
	}
}
