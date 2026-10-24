use anyhow::{Context, Result};
use octocrab::OctocrabBuilder;
use open::that as open_in_browser;

use crate::{
	config::Config,
	utils::{ask, confirm},
};

const TOKEN_URL: &str = "https://github.com/settings/tokens/new?scopes=repo&description=gitkeep";

pub async fn run() -> Result<()> {
	if confirm("Open GitHub token settings in browser?", false)? && open_in_browser(TOKEN_URL).is_err() {
		println!("Could not open browser. Please visit {TOKEN_URL}.");
	}
	let token = loop {
		let token = ask("Paste your token", None)?;
		if !token.is_empty() {
			break token;
		}
		println!("Token cannot be empty.");
	};
	println!("Validating token with GitHub...");
	let client =
		OctocrabBuilder::default().personal_token(token.clone()).build().context("Could not create GitHub client")?;
	let user =
		client.current().user().await.context("Token validation failed. The token may be invalid or expired.")?;
	println!("Authenticated as {}.", user.login);
	let mut config = Config::load()?;
	config.token = Some(token);
	let already_tracked = config.track.iter().any(|u| u.name.eq_ignore_ascii_case(&user.login));
	if already_tracked || confirm(&format!("Track your own account ({})?", user.login), true)? {
		config.add_account(None, &user.login, false, false, None);
	}
	config.save()?;
	println!("Token saved to {}.", Config::path()?.display());
	Ok(())
}
