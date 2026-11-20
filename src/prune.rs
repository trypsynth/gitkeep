use std::{
	collections::HashSet,
	fs,
	path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use crate::{
	config::Config,
	size::{SizeFormat, dir_size, format_size, repo_dirs},
	sync,
	utils::{confirm, plural},
};

/// Deletes the local copies of repos nothing tracked lists any more: repos gone upstream, removed
/// from a tracked account, unstarred, or left from an account that's no longer tracked. Asks first
/// unless `yes`.
pub async fn run(yes: bool) -> Result<()> {
	let archive_dir = Config::load()?.archive_dir()?;
	if !archive_dir.exists() {
		println!("Archive directory {} does not exist yet.", archive_dir.display());
		return Ok(());
	}
	let kept = sync::kept_repos().await?;
	let untracked = untracked(&archive_dir, &kept)?;
	if untracked.is_empty() {
		println!("Nothing to prune.");
		return Ok(());
	}
	let mut total = 0;
	println!("Not tracked any more:");
	for (name, _, size) in &untracked {
		total += size;
		println!("{name}: {}", format_size(*size, SizeFormat::Binary));
	}
	let what = format!("{} ({})", plural(untracked.len(), "repo", "repos"), format_size(total, SizeFormat::Binary));
	if !yes && !confirm(&format!("Delete {what}?"), false)? {
		println!("Nothing was deleted.");
		return Ok(());
	}
	for (_, path, _) in &untracked {
		fs::remove_dir_all(path).with_context(|| format!("Could not delete {}", path.display()))?;
		remove_empty_parents(path, &archive_dir);
	}
	println!("Deleted {what}.");
	Ok(())
}

/// The repos in the archive that aren't in `kept`, with their paths and sizes, in name order. Names
/// are compared ignoring case: on a case-insensitive filesystem a sync keeps using a folder after
/// its repo's name changes case upstream, so that folder is still the repo's.
fn untracked(archive_dir: &Path, kept: &HashSet<String>) -> Result<Vec<(String, PathBuf, u64)>> {
	let kept: HashSet<String> = kept.iter().map(|k| k.to_lowercase()).collect();
	let mut repos: Vec<(String, PathBuf, u64)> = repo_dirs(archive_dir)?
		.into_iter()
		.filter(|(name, _)| !kept.contains(&name.to_lowercase()))
		.map(|(name, path)| {
			let size = dir_size(&path)?;
			Ok((name, path, size))
		})
		.collect::<Result<_>>()?;
	repos.sort_by(|a, b| a.0.cmp(&b.0));
	Ok(repos)
}

/// Removes the folders that held `path` and are now empty, such as an account's folder after its
/// last repo went, stopping at the archive itself.
fn remove_empty_parents(path: &Path, archive_dir: &Path) {
	for dir in path.ancestors().skip(1).take_while(|d| *d != archive_dir) {
		if fs::remove_dir(dir).is_err() {
			break;
		}
	}
}

#[cfg(test)]
mod tests {
	use std::{env, process};

	use super::*;

	#[test]
	fn untracked_lists_repos_missing_from_kept_and_cleans_their_empty_folders() {
		let root = env::temp_dir().join(format!("gitkeep-prune-test-{}", process::id()));
		for dir in ["alice/kept", "alice/gone", "bob/old", "host.example.com/grp/sub/proj"] {
			fs::create_dir_all(root.join(dir).join(".git")).unwrap();
		}
		let kept: HashSet<String> = ["Alice/Kept".to_string()].into();
		let names: Vec<String> = untracked(&root, &kept).unwrap().into_iter().map(|r| r.0).collect();
		assert_eq!(names, ["alice/gone", "bob/old", "host.example.com/grp/sub/proj"]);
		let proj = root.join("host.example.com/grp/sub/proj");
		fs::remove_dir_all(&proj).unwrap();
		remove_empty_parents(&proj, &root);
		let still_there = root.exists() && root.join("alice/kept").exists();
		let host_gone = !root.join("host.example.com").exists();
		fs::remove_dir_all(&root).unwrap();
		assert!(still_there && host_gone);
	}
}
