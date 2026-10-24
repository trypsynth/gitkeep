use std::path::{self, Path};

use anyhow::{Context, Result};
use dirs::home_dir;

use crate::{
	config::Config,
	utils::{ask, confirm},
};

pub fn run() -> Result<()> {
	let path = Config::path()?;
	let mut config = Config::load()?;
	if path.exists() {
		println!("Config found at {}. Tracked users and your auth token will not be affected.", path.display());
		if !confirm("Update settings?", false)? {
			println!("No changes made.");
			return Ok(());
		}
	}
	let default_archive = config.archive_dir.clone().unwrap_or_else(|| {
		home_dir().map_or_else(|| "~/gitkeep".to_string(), |h| h.join("gitkeep").to_string_lossy().into_owned())
	});
	let archive_dir = Some(ask("Archive directory", Some(&default_archive))?).filter(|s| !s.is_empty());
	config.archive_dir = archive_dir.map(|dir| resolve_archive_dir(&dir)).transpose()?;
	config.use_ssh = confirm("Use SSH clone URLs?", config.use_ssh)?;
	config.submodules = confirm("Clone submodules by default?", config.submodules)?;
	config.no_sync = !confirm("Clone repos immediately after adding them?", !config.no_sync)?;
	config.save()?;
	println!("Config written to {}.", path.display());
	if config.token.is_none() {
		println!("Tip: Run 'gitkeep login' to authenticate with GitHub.");
	}
	Ok(())
}

/// Pin a relative archive directory to the current directory so later runs don't depend on where they start.
fn resolve_archive_dir(dir: &str) -> Result<String> {
	if dir == "~" || dir.starts_with("~/") || Path::new(dir).is_absolute() {
		return Ok(dir.to_string());
	}
	let abs = path::absolute(dir).with_context(|| format!("Could not resolve {dir} to an absolute path"))?;
	Ok(abs.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
	use std::env::current_dir;

	use super::*;

	#[test]
	fn resolve_archive_dir_keeps_absolute_and_tilde_paths() {
		assert_eq!(resolve_archive_dir("/srv/gh").unwrap(), "/srv/gh");
		assert_eq!(resolve_archive_dir("~/gh").unwrap(), "~/gh");
		assert_eq!(resolve_archive_dir("~").unwrap(), "~");
	}

	#[test]
	fn resolve_archive_dir_anchors_relative_paths_to_current_dir() {
		let expected = current_dir().unwrap().join("gh");
		assert_eq!(resolve_archive_dir("gh").unwrap(), expected.to_string_lossy());
	}
}
