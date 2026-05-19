use anyhow::Result;

use crate::config::State;

pub fn add(repos: &[String]) -> Result<()> {
	let mut state = State::load()?;
	for repo in repos {
		state.skip_repo(repo);
		println!("Ignoring {repo}.");
	}
	state.save()
}

pub fn remove(repos: &[String]) -> Result<()> {
	let mut state = State::load()?;
	for repo in repos {
		state.unskip_repo(repo);
		println!("No longer ignoring {repo}.");
	}
	state.save()
}
