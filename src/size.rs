use std::{
	fmt::Write as _,
	fs,
	path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;

use crate::{
	config::{Config, TrackedAccount},
	forge::archive_path,
};

#[derive(Clone, Copy, ValueEnum)]
pub enum SizeFormat {
	/// Powers of 1000 (kB, MB, GB, ...)
	Decimal,
	/// Powers of 1024 (KiB, MiB, GiB, ...)
	Binary,
	/// Exact byte count, no unit conversion
	Raw,
}

pub fn run(account: Option<&str>, format: SizeFormat, breakdown: bool) -> Result<()> {
	let config = Config::load()?;
	let archive_dir = config.archive_dir()?;
	if !archive_dir.exists() {
		println!("Archive directory {} does not exist yet.", archive_dir.display());
		return Ok(());
	}
	let (entries, label) = match account {
		Some(arg) => {
			let key = account_key(&config, arg);
			let dir = archive_path(&archive_dir, &key);
			if !dir.is_dir() {
				bail!("Nothing is archived for {key}.");
			}
			(repo_sizes(&dir)?, "repo")
		}
		None => (account_sizes(&archive_dir)?, "account"),
	};
	if entries.is_empty() {
		println!("Archive is empty.");
		return Ok(());
	}
	println!("{}", build_report(entries, label, format, breakdown));
	Ok(())
}

/// The archive key `arg` names, spelled as the tracked account is, since names are matched without
/// regard to case but the archive's folders aren't.
fn account_key(config: &Config, arg: &str) -> String {
	let target = config.target(arg);
	config
		.track
		.iter()
		.find(|u| u.host == target.host && u.name.eq_ignore_ascii_case(&target.path))
		.map_or_else(|| target.key(), TrackedAccount::display_name)
}

/// Measures each repo (each folder with a `.git` in it) under `dir`, named by its path inside it. A
/// repo folder itself is measured as one entry named after the folder.
fn repo_sizes(dir: &Path) -> Result<Vec<(String, u64)>> {
	if dir.join(".git").exists() {
		let name = dir.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
		return Ok(vec![(name, dir_size(dir)?)]);
	}
	let mut entries = Vec::new();
	for (name, path) in subdirs(dir)? {
		if path.join(".git").exists() {
			entries.push((name, dir_size(&path)?));
		} else {
			entries.extend(repo_sizes(&path)?.into_iter().map(|(sub, size)| (format!("{name}/{sub}"), size)));
		}
	}
	Ok(entries)
}

/// Measures each account directory in the archive. Archives from forges other than GitHub sit under
/// a host directory (its name has a dot, which GitHub names can't), so those are measured one level
/// down and named `host/owner`.
fn account_sizes(archive_dir: &Path) -> Result<Vec<(String, u64)>> {
	let mut entries = Vec::new();
	for (name, path) in subdirs(archive_dir)? {
		if name.contains('.') {
			for (owner, owner_path) in subdirs(&path)? {
				entries.push((format!("{name}/{owner}"), dir_size(&owner_path)?));
			}
		} else {
			entries.push((name, dir_size(&path)?));
		}
	}
	Ok(entries)
}

fn subdirs(path: &Path) -> Result<Vec<(String, PathBuf)>> {
	let mut dirs = Vec::new();
	for entry in fs::read_dir(path).with_context(|| format!("Could not read directory {}", path.display()))? {
		let entry = entry?;
		if entry.file_type()?.is_dir() {
			dirs.push((entry.file_name().to_string_lossy().into_owned(), entry.path()));
		}
	}
	Ok(dirs)
}

/// The total size alone, or with `breakdown`, each entry's size, largest first, then the total.
/// `label` is what the entries are, as in "Per-account sizes".
fn build_report(mut entries: Vec<(String, u64)>, label: &str, format: SizeFormat, breakdown: bool) -> String {
	let total = format_size(entries.iter().map(|(_, size)| size).sum(), format);
	if !breakdown {
		return total;
	}
	entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
	let width = entries.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
	let mut out = format!("Per-{label} sizes:\n");
	for (name, size) in &entries {
		let _ = writeln!(out, "{name:width$}  {}", format_size(*size, format));
	}
	let _ = write!(out, "\nTotal: {total}");
	out
}

/// Sums the apparent size of every regular file under `path`. Symlinks aren't followed, so
/// symlink cycles can't loop forever.
fn dir_size(path: &Path) -> Result<u64> {
	let mut total = 0u64;
	for entry in fs::read_dir(path).with_context(|| format!("Could not read directory {}", path.display()))? {
		let entry = entry?;
		let file_type = entry.file_type()?;
		if file_type.is_dir() {
			total += dir_size(&entry.path())?;
		} else if file_type.is_file() {
			total += entry.metadata()?.len();
		}
	}
	Ok(total)
}

fn format_size(bytes: u64, format: SizeFormat) -> String {
	match format {
		SizeFormat::Raw => format!("{bytes} bytes"),
		SizeFormat::Decimal => format_with_units(bytes, 1000.0, &["B", "kB", "MB", "GB", "TB", "PB"]),
		SizeFormat::Binary => format_with_units(bytes, 1024.0, &["B", "KiB", "MiB", "GiB", "TiB", "PiB"]),
	}
}

fn format_with_units(bytes: u64, base: f64, units: &[&str]) -> String {
	#[allow(clippy::cast_precision_loss)]
	let mut value = bytes as f64;
	let mut unit = 0;
	while value >= base && unit < units.len() - 1 {
		value /= base;
		unit += 1;
	}
	if unit == 0 { format!("{value:.0} {}", units[unit]) } else { format!("{value:.2} {}", units[unit]) }
}

#[cfg(test)]
mod tests {
	use std::{env, process};

	use super::*;

	fn entries() -> Vec<(String, u64)> {
		vec![("bob".into(), 100), ("alice".into(), 300)]
	}

	#[test]
	fn account_sizes_splits_host_directories_by_owner() {
		let root = env::temp_dir().join(format!("gitkeep-size-test-{}", process::id()));
		for (dir, bytes) in [("alice/a", 10), ("gitlab.example.com/grp/sub/b", 20), ("codeberg.org/bob/c", 30)] {
			fs::create_dir_all(root.join(dir)).unwrap();
			fs::write(root.join(dir).join("f"), vec![0; bytes]).unwrap();
		}
		let mut entries = account_sizes(&root).unwrap();
		entries.sort();
		fs::remove_dir_all(&root).unwrap();
		assert_eq!(
			entries,
			[("alice".into(), 10), ("codeberg.org/bob".into(), 30), ("gitlab.example.com/grp".into(), 20)]
		);
	}

	#[test]
	fn repo_sizes_finds_repos_at_any_depth_and_skips_other_folders() {
		let root = env::temp_dir().join(format!("gitkeep-repo-size-test-{}", process::id()));
		for (dir, bytes) in [("a/.git", 10), ("a.wiki/.git", 5), ("sub/b/.git", 20), ("loose", 7)] {
			fs::create_dir_all(root.join(dir)).unwrap();
			fs::write(root.join(dir).join("f"), vec![0; bytes]).unwrap();
		}
		let mut entries = repo_sizes(&root).unwrap();
		let single = repo_sizes(&root.join("a")).unwrap();
		fs::remove_dir_all(&root).unwrap();
		entries.sort();
		assert_eq!(entries, [("a".into(), 10), ("a.wiki".into(), 5), ("sub/b".into(), 20)]);
		assert_eq!(single, [("a".into(), 10)]);
	}

	#[test]
	fn report_shows_only_total_by_default() {
		assert_eq!(build_report(entries(), "account", SizeFormat::Raw, false), "400 bytes");
	}

	#[test]
	fn report_breakdown_lists_accounts_largest_first_under_a_header() {
		assert_eq!(
			build_report(entries(), "account", SizeFormat::Raw, true),
			"Per-account sizes:\nalice  300 bytes\nbob    100 bytes\n\nTotal: 400 bytes"
		);
	}

	#[test]
	fn raw_format_shows_exact_bytes() {
		assert_eq!(format_size(12345, SizeFormat::Raw), "12345 bytes");
	}

	#[test]
	fn decimal_format_uses_1000_base() {
		assert_eq!(format_size(1_000_000, SizeFormat::Decimal), "1.00 MB");
	}

	#[test]
	fn binary_format_uses_1024_base() {
		assert_eq!(format_size(1024 * 1024, SizeFormat::Binary), "1.00 MiB");
	}

	#[test]
	fn small_sizes_stay_in_bytes() {
		assert_eq!(format_size(512, SizeFormat::Decimal), "512 B");
		assert_eq!(format_size(512, SizeFormat::Binary), "512 B");
	}

	#[test]
	fn decimal_and_binary_diverge_for_large_sizes() {
		let bytes = 5 * 1024 * 1024 * 1024;
		assert_eq!(format_size(bytes, SizeFormat::Decimal), "5.37 GB");
		assert_eq!(format_size(bytes, SizeFormat::Binary), "5.00 GiB");
	}

	#[test]
	fn zero_bytes_formats_cleanly() {
		assert_eq!(format_size(0, SizeFormat::Decimal), "0 B");
		assert_eq!(format_size(0, SizeFormat::Raw), "0 bytes");
	}

	#[test]
	fn caps_at_largest_unit() {
		let huge = 3_000_000_000_000_000_000u64;
		assert_eq!(format_size(huge, SizeFormat::Decimal), "3000.00 PB");
	}
}
