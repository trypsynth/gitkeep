use anyhow::{Context, Result, bail};
use octocrab::OctocrabBuilder;

use crate::config::{Config, TrackedUser};
use crate::utils::prompt;

const TOKEN_URL: &str = "https://github.com/settings/tokens/new?scopes=repo&description=gitkeep";

pub async fn run() -> Result<()> {
	let answer = prompt("Open GitHub token settings in browser? [y/N]: ")?;
	if answer.trim().eq_ignore_ascii_case("y") {
		if open::that(TOKEN_URL).is_err() {
			println!("Could not open browser. Please visit {TOKEN_URL}");
		}
	}
	let raw = prompt("Paste your token: ")?;
	let token = raw.trim().to_string();
	if token.is_empty() {
		bail!("Token cannot be empty. Please run 'gitkeep login' again.");
	}
	println!("Validating token with GitHub...");
	let client =
		OctocrabBuilder::default().personal_token(token.clone()).build().context("Could not create GitHub client")?;
	let user =
		client.current().user().await.context("Token validation failed. The token may be invalid or expired.")?;
	println!("Authenticated as {}.", user.login);
	let mut config = Config::load()?;
	config.token = Some(token);
	if !config.is_tracked(&user.login) {
		config.track.push(TrackedUser::new(user.login.clone()));
		println!("Added {} to tracked users.", user.login);
	}
	config.save()?;
	println!("Token saved to {}.", Config::path()?.display());
	Ok(())
}
