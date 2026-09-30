use std::fmt::Write as _;

use anyhow::Result;

use crate::config::{Config, PinnedRepo};

fn format_list(config: &Config) -> String {
	let mut out = String::new();
	if config.track.is_empty() && config.pinned.is_empty() {
		return "No users tracked. Use 'gitkeep add <username>' to start.\n".to_string();
	}
	if !config.track.is_empty() {
		let _ = writeln!(out, "Tracked users and orgs ({} total):", config.track.len());
		for user in &config.track {
			let mut tags = Vec::new();
			if user.forks {
				tags.push("forks");
			}
			if user.frozen {
				tags.push("frozen");
			}
			match user.submodules {
				Some(true) => tags.push("submodules"),
				Some(false) => tags.push("no-submodules"),
				None => {}
			}
			let suffix = if tags.is_empty() { String::new() } else { format!(" [{}]", tags.join(", ")) };
			let _ = writeln!(out, "  {}{}", user.display_name(), suffix);
		}
	}
	if !config.pinned.is_empty() {
		let mut sorted: Vec<&PinnedRepo> = config.pinned.iter().collect();
		sorted.sort_by(|a, b| a.full_name.cmp(&b.full_name));
		if !out.is_empty() {
			out.push('\n');
		}
		let _ = writeln!(out, "Repos ({} total):", sorted.len());
		for repo in sorted {
			let suffix = match repo.submodules {
				Some(true) => " [submodules]",
				Some(false) => " [no-submodules]",
				None => "",
			};
			let _ = writeln!(out, "  {}{}", repo.full_name, suffix);
		}
	}
	if !config.excluded.is_empty() {
		let mut sorted: Vec<&String> = config.excluded.iter().collect();
		sorted.sort_by_key(|r| r.to_lowercase());
		let _ = writeln!(
			out,
			"
Removed repos ({} total):",
			sorted.len()
		);
		for repo in sorted {
			let _ = writeln!(out, "  {repo}");
		}
	}
	out
}

pub fn list() -> Result<()> {
	let config = Config::load()?;
	print!("{}", format_list(&config));
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn list_empty_state_shows_hint() {
		let config = Config::default();
		let out = format_list(&config);
		assert!(out.contains("gitkeep add"), "got: {out}");
	}

	#[test]
	fn list_pinned_only_does_not_show_hint() {
		let mut config = Config::default();
		config.pin_repo_with_options("alice/repo", None, None);
		let out = format_list(&config);
		assert!(!out.contains("gitkeep add"), "got: {out}");
	}

	#[test]
	fn list_shows_tracked_users() {
		let mut config = Config::default();
		config.add_account(None, "alice", false, false, None);
		let out = format_list(&config);
		assert!(out.contains("alice"), "got: {out}");
	}

	#[test]
	fn list_shows_forks_tag() {
		let mut config = Config::default();
		config.add_account(None, "alice", true, false, None);
		let out = format_list(&config);
		assert!(out.contains("forks"), "got: {out}");
	}

	#[test]
	fn list_shows_frozen_tag() {
		let mut config = Config::default();
		config.add_account(None, "alice", false, true, None);
		let out = format_list(&config);
		assert!(out.contains("frozen"), "got: {out}");
	}

	#[test]
	fn list_omits_removed_section_when_none() {
		let mut config = Config::default();
		config.add_account(None, "alice", false, false, None);
		let out = format_list(&config);
		assert!(!out.to_lowercase().contains("removed"), "got: {out}");
	}

	#[test]
	fn list_shows_removed_section_when_present() {
		let mut config = Config::default();
		config.add_account(None, "alice", false, false, None);
		config.exclude_repo("alice/noisy");
		let out = format_list(&config);
		assert!(out.contains("alice/noisy"), "got: {out}");
		assert!(out.to_lowercase().contains("removed"), "got: {out}");
	}

	#[test]
	fn list_removed_repos_are_sorted() {
		let mut config = Config::default();
		config.add_account(None, "alice", false, false, None);
		config.exclude_repo("alice/zzz");
		config.exclude_repo("alice/aaa");
		let out = format_list(&config);
		let aaa_pos = out.find("alice/aaa").unwrap();
		let zzz_pos = out.find("alice/zzz").unwrap();
		assert!(aaa_pos < zzz_pos, "got: {out}");
	}

	#[test]
	fn list_shows_pinned_section() {
		let mut config = Config::default();
		config.pin_repo_with_options("rust-lang/mdBook", None, None);
		let out = format_list(&config);
		assert!(out.contains("rust-lang/mdBook"), "got: {out}");
		assert!(out.contains("Repos ("), "got: {out}");
	}

	#[test]
	fn list_shows_submodules_tag_when_enabled() {
		let mut config = Config::default();
		config.add_account(None, "alice", false, false, Some(true));
		let out = format_list(&config);
		assert!(out.contains("submodules"), "got: {out}");
	}

	#[test]
	fn list_shows_no_submodules_tag_when_explicitly_disabled() {
		let mut config = Config::default();
		config.add_account(None, "alice", false, false, Some(false));
		let out = format_list(&config);
		assert!(out.contains("no-submodules"), "got: {out}");
	}

	#[test]
	fn list_omits_submodules_tag_when_unset() {
		let mut config = Config::default();
		config.add_account(None, "alice", false, false, None);
		let out = format_list(&config);
		assert!(!out.contains("submodules"), "got: {out}");
	}

	#[test]
	fn list_shows_submodules_tag_for_pinned_repo() {
		let mut config = Config::default();
		config.pin_repo_with_options("alice/repo", None, Some(true));
		let out = format_list(&config);
		assert!(out.contains("alice/repo"), "got: {out}");
		assert!(out.contains("submodules"), "got: {out}");
	}

	#[test]
	fn list_pinned_repos_are_sorted() {
		let mut config = Config::default();
		config.pin_repo_with_options("rust-lang/zzz", None, None);
		config.pin_repo_with_options("rust-lang/aaa", None, None);
		let out = format_list(&config);
		let aaa_pos = out.find("rust-lang/aaa").unwrap();
		let zzz_pos = out.find("rust-lang/zzz").unwrap();
		assert!(aaa_pos < zzz_pos, "got: {out}");
	}

	#[test]
	fn list_omits_pinned_section_when_none() {
		let mut config = Config::default();
		config.add_account(None, "alice", false, false, None);
		let out = format_list(&config);
		assert!(!out.contains("Repos ("), "got: {out}");
	}

	#[test]
	fn list_shows_host_qualified_gitlab_entries() {
		let mut config = Config::default();
		config.add_account(Some("gitlab.example.com"), "some-group", false, false, None);
		let out = format_list(&config);
		assert!(out.contains("gitlab.example.com/some-group"), "got: {out}");
	}
}
