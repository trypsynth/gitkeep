use anyhow::Result;
use dirs::home_dir;

use crate::config::Config;
use crate::utils::prompt;

pub fn run() -> Result<()> {
	let path = Config::path()?;
	let existing = Config::load()?;
	if path.exists() {
		println!("Config already exists at {}.", path.display());
		let answer = prompt("Overwrite? [y/N]: ")?;
		if !answer.trim().eq_ignore_ascii_case("y") {
			println!("Keeping existing config. No changes made.");
			return Ok(());
		}
	}
	let default_archive = existing.archive_dir.clone().unwrap_or_else(|| {
		home_dir().map(|h| h.join("gitkeep").to_string_lossy().into_owned()).unwrap_or_else(|| "~/gitkeep".to_string())
	});
	let archive_input = prompt(&format!("Archive directory [{}]: ", default_archive))?;
	let archive_dir = match archive_input.trim() {
		"" => existing.archive_dir,
		s => Some(s.to_string()),
	};
	let ssh_hint = if existing.use_ssh { "Y/n" } else { "y/N" };
	let ssh_input = prompt(&format!("Use SSH clone URLs? [{}]: ", ssh_hint))?;
	let use_ssh = match ssh_input.trim().to_ascii_lowercase().as_str() {
		"y" | "yes" => true,
		"n" | "no" => false,
		_ => existing.use_ssh,
	};
	let config = Config { token: existing.token, archive_dir, use_ssh, track: existing.track };
	config.save()?;
	println!("Config written to {}.", path.display());
	if config.token.is_none() {
		println!("Tip: Run 'gitkeep login' to authenticate with GitHub.");
	}
	Ok(())
}
