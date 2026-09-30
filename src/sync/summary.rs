use std::fmt::Write as _;

use crate::utils::plural;

#[derive(Default)]
pub struct Totals {
	pub pulled_updated: usize,
	pub pulled_up_to_date: usize,
	pub cloned: usize,
	pub excluded: usize,
	pub failed: usize,
	pub updated_repos: Vec<String>,
	pub new_repos: Vec<String>,
}

pub fn build_summary(totals: &Totals) -> String {
	let total_processed = totals.pulled_updated + totals.pulled_up_to_date + totals.cloned + totals.failed;
	if total_processed == 0 {
		return if totals.excluded > 0 { "Done.".to_string() } else { "Nothing to do.".to_string() };
	}
	let mut parts: Vec<String> = Vec::new();
	if totals.cloned > 0 {
		parts.push(format!("{} cloned", plural(totals.cloned, "new repo", "new repos")));
	}
	if totals.pulled_updated > 0 {
		parts.push(format!("{} with new commits", plural(totals.pulled_updated, "repo", "repos")));
	}
	if totals.pulled_up_to_date > 0 {
		parts.push(format!("{} up to date", plural(totals.pulled_up_to_date, "repo", "repos")));
	}
	if totals.failed > 0 {
		parts.push(format!("{} failed", plural(totals.failed, "repo", "repos")));
	}
	format!("Done. {}.", parts.join(", "))
}

pub fn build_normal_detail(totals: &Totals) -> Option<String> {
	if totals.new_repos.is_empty() && totals.updated_repos.is_empty() {
		return None;
	}
	let mut out = String::new();
	if !totals.new_repos.is_empty() {
		out.push_str("Cloned:\n");
		for r in &totals.new_repos {
			let _ = writeln!(out, "  {r}");
		}
	}
	if !totals.updated_repos.is_empty() {
		if !out.is_empty() {
			out.push('\n');
		}
		out.push_str("Updated:\n");
		for r in &totals.updated_repos {
			let _ = writeln!(out, "  {r}");
		}
	}
	Some(out.trim_end().to_string())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn summary_nothing_to_do_when_truly_empty() {
		let s = build_summary(&Totals::default());
		assert_eq!(s, "Nothing to do.");
	}

	#[test]
	fn summary_does_not_show_excluded() {
		let s = build_summary(&Totals { excluded: 5, ..Totals::default() });
		assert!(!s.contains("excluded"), "got: {s}");
		assert!(!s.contains("skipped"), "got: {s}");
	}

	#[test]
	fn summary_shows_done_when_only_excluded() {
		let s = build_summary(&Totals { excluded: 5, ..Totals::default() });
		assert_eq!(s, "Done.");
	}

	#[test]
	fn detail_empty_when_nothing_notable() {
		let totals = Totals { pulled_up_to_date: 5, ..Totals::default() };
		assert!(build_normal_detail(&totals).is_none());
	}

	#[test]
	fn detail_shows_cloned_section() {
		let totals = Totals { new_repos: vec!["alice/fresh".to_string()], cloned: 1, ..Totals::default() };
		let detail = build_normal_detail(&totals).unwrap();
		assert!(detail.contains("Cloned"), "got: {detail}");
		assert!(detail.contains("alice/fresh"), "got: {detail}");
	}

	#[test]
	fn detail_shows_updated_section() {
		let totals = Totals { updated_repos: vec!["alice/old".to_string()], pulled_updated: 1, ..Totals::default() };
		let detail = build_normal_detail(&totals).unwrap();
		assert!(detail.contains("Updated"), "got: {detail}");
		assert!(detail.contains("alice/old"), "got: {detail}");
	}

	#[test]
	fn detail_omits_empty_sections() {
		let totals = Totals { updated_repos: vec!["alice/repo".to_string()], pulled_updated: 1, ..Totals::default() };
		let detail = build_normal_detail(&totals).unwrap();
		assert!(!detail.contains("Cloned"), "got: {detail}");
	}

	#[test]
	fn detail_shows_both_sections_when_populated() {
		let totals = Totals {
			new_repos: vec!["alice/new".to_string()],
			updated_repos: vec!["alice/old".to_string()],
			cloned: 1,
			pulled_updated: 1,
			..Totals::default()
		};
		let detail = build_normal_detail(&totals).unwrap();
		assert!(detail.contains("Cloned"), "got: {detail}");
		assert!(detail.contains("Updated"), "got: {detail}");
	}
}
