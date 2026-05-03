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

/// A single-key confirmation prompt that returns immediately on 'y' or 'n'.
pub fn confirm(message: &str, default: bool) -> Result<bool> {
	let hint = if default { "[Y/n]" } else { "[y/N]" };
	// Format to look similar to inquire prompts
	print!("{} {} {} ", "?".cyan(), message.bold(), hint.dark_grey());
	io::stdout().flush().context("Could not flush stdout")?;
	enable_raw_mode().context("Could not enable raw mode")?;
	let result = loop {
		match event::read() {
			Ok(Event::Key(key)) => {
				// Only handle press events to avoid double-processing on some platforms
				if key.kind == KeyEventKind::Release {
					continue;
				}
				match key.code {
					KeyCode::Char('y' | 'Y') => break Ok(true),
					KeyCode::Char('n' | 'N') => break Ok(false),
					KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
						break Err(anyhow::anyhow!("Interrupted by user"));
					}
					KeyCode::Esc => break Ok(false),
					_ => {}
				}
			}
			Ok(_) => {}
			Err(e) => break Err(anyhow::Error::from(e)),
		}
	};
	disable_raw_mode().ok(); // Best effort to disable raw mode
	match result {
		Ok(val) => {
			println!("{}", if val { "Yes".cyan() } else { "No".cyan() });
			Ok(val)
		}
		Err(e) => {
			println!(); // Ensure we move to the next line on error/interrupt
			Err(e)
		}
	}
}
