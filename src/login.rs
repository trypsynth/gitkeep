use anyhow::{Context, Result};
use inquire::{Text, validator::Validation};
use octocrab::OctocrabBuilder;

use crate::{config::Config, utils::confirm};

const TOKEN_URL: &str = "https://github.com/settings/tokens/new?scopes=repo&description=gitkeep";

pub async fn run() -> Result<()> {
	if confirm("Open GitHub token settings in browser?", false)? && open::that(TOKEN_URL).is_err() {
		println!("Could not open browser. Please visit {TOKEN_URL}.");
	}
	let token = Text::new("Paste your token:")
		.with_validator(|s: &str| {
			if s.trim().is_empty() {
				Ok(Validation::Invalid("Token cannot be empty.".into()))
			} else {
				Ok(Validation::Valid)
			}
		})
		.prompt()?
		.trim()
		.to_string();
	println!("Validating token with GitHub...");
	let client =
		OctocrabBuilder::default().personal_token(token.clone()).build().context("Could not create GitHub client")?;
	let user =
		client.current().user().await.context("Token validation failed. The token may be invalid or expired.")?;
	println!("Authenticated as {}.", user.login);
	let mut config = Config::load()?;
	config.token = Some(token);
	config.add_user(&user.login, false, false, None);
	config.save()?;
	println!("Token saved to {}.", Config::path()?.display());
	Ok(())
}
