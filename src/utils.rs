use std::io::{self, Write};

use anyhow::{Context, Result, bail};
use crossterm::{
	event::{self, Event, KeyCode, KeyEventKind},
	style::Stylize,
	terminal::{disable_raw_mode, enable_raw_mode},
};

pub fn plural(n: usize, singular: &str, plural: &str) -> String {
	if n == 1 { format!("1 {singular}") } else { format!("{n} {plural}") }
}

/// Expands `owner/a,b,c` into one `owner/name` target per repo (repo names can't contain commas).
/// The list follows the last `/`, so `host/group/a,b` works for GitLab too. Other targets pass
/// through unchanged.
pub fn expand_targets(args: Vec<String>) -> Result<Vec<String>> {
	let mut out = Vec::with_capacity(args.len());
	for target in args {
		let Some((owner, list)) = target.rsplit_once('/').filter(|(_, rest)| rest.contains(',')) else {
			out.push(target);
			continue;
		};
		let names: Vec<&str> = list.split(',').map(str::trim).filter(|n| !n.is_empty()).collect();
		if names.is_empty() {
			bail!("'{target}' doesn't list any repos");
		}
		out.extend(names.into_iter().map(|name| format!("{owner}/{name}")));
	}
	Ok(out)
}

/// A single-key confirmation prompt that returns immediately on 'y', 'n', or Enter.
pub fn confirm(message: &str, default: bool) -> Result<bool> {
	let hint = if default { "[Y/n]" } else { "[y/N]" };
	// Format to look similar to inquire prompts
	print!("{} {} {} ", "?".cyan(), message.bold(), hint.dark_grey());
	io::stdout().flush().context("Could not flush stdout")?;
	enable_raw_mode().context("Could not enable raw mode")?;
	let result = loop {
		match event::read() {
			Ok(Event::Key(key)) => {
				if key.kind == KeyEventKind::Release {
					continue;
				}
				match key.code {
					KeyCode::Char('y' | 'Y') => break Ok(true),
					KeyCode::Char('n' | 'N') | KeyCode::Esc => break Ok(false),
					KeyCode::Enter => break Ok(default),
					KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
						break Err(anyhow::anyhow!("Interrupted by user"));
					}
					_ => {}
				}
			}
			Ok(_) => {}
			Err(e) => break Err(anyhow::Error::from(e)),
		}
	};
	disable_raw_mode().ok();
	match result {
		Ok(val) => {
			println!("{}", if val { "Yes".cyan() } else { "No".cyan() });
			Ok(val)
		}
		Err(e) => {
			println!();
			Err(e)
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn args(list: &[&str]) -> Vec<String> {
		list.iter().map(ToString::to_string).collect()
	}

	#[test]
	fn expand_targets_passes_through_plain_targets() {
		let input = args(&["alice", "bob/repo"]);
		assert_eq!(expand_targets(input.clone()).unwrap(), input);
	}

	#[test]
	fn expand_targets_expands_comma_list() {
		assert_eq!(expand_targets(args(&["daisy/a,b,c"])).unwrap(), args(&["daisy/a", "daisy/b", "daisy/c"]));
	}

	#[test]
	fn expand_targets_keeps_nested_owner_path() {
		assert_eq!(
			expand_targets(args(&["gitlab.example.com/grp/sub/a,b"])).unwrap(),
			args(&["gitlab.example.com/grp/sub/a", "gitlab.example.com/grp/sub/b"])
		);
	}

	#[test]
	fn expand_targets_ignores_empty_entries() {
		assert_eq!(expand_targets(args(&["daisy/a,,b,"])).unwrap(), args(&["daisy/a", "daisy/b"]));
	}

	#[test]
	fn expand_targets_rejects_empty_list() {
		assert!(expand_targets(args(&["daisy/,"])).is_err());
	}

	#[test]
	fn plural_uses_singular_for_one() {
		assert_eq!(plural(1, "repo", "repos"), "1 repo");
	}

	#[test]
	fn plural_uses_plural_for_zero() {
		assert_eq!(plural(0, "repo", "repos"), "0 repos");
	}

	#[test]
	fn plural_uses_plural_for_many() {
		assert_eq!(plural(5, "repo", "repos"), "5 repos");
	}
}
