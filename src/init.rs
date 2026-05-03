use anyhow::Result;
use dirs::home_dir;
use inquire::Text;

use crate::{config::Config, utils::confirm};

pub fn run() -> Result<()> {
	let path = Config::path()?;
	let existing = Config::load()?;
	if path.exists() {
		println!("Config already exists at {}.", path.display());
		if !confirm("Overwrite?", false)? {
			println!("Keeping existing config. No changes made.");
			return Ok(());
		}
	}
	let default_archive = existing.archive_dir.clone().unwrap_or_else(|| {
		home_dir().map_or_else(|| "~/gitkeep".to_string(), |h| h.join("gitkeep").to_string_lossy().into_owned())
	});
	let archive_dir = Text::new("Archive directory")
		.with_default(&default_archive)
		.prompt()
		.map(|s| if s.trim().is_empty() { None } else { Some(s.trim().to_string()) })?;
	let use_ssh = confirm("Use SSH clone URLs?", existing.use_ssh)?;
	let config = Config { token: existing.token, archive_dir, use_ssh, track: existing.track };
	config.save()?;
	println!("Config written to {}.", path.display());
	if config.token.is_none() {
		println!("Tip: Run 'gitkeep login' to authenticate with GitHub.");
	}
	Ok(())
}
