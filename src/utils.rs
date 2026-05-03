use std::io::{self, Write};

use anyhow::{Context, Result};
use crossterm::{
	event::{self, Event, KeyCode, KeyEventKind},
	style::Stylize,
	terminal::{disable_raw_mode, enable_raw_mode},
};

pub fn plural(n: usize, singular: &str, plural: &str) -> String {
	if n == 1 { format!("1 {singular}") } else { format!("{n} {plural}") }
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
