use std::io::{self, Write};
use anyhow::{Context, Result};

pub fn plural(n: usize, singular: &str, plural: &str) -> String {
	if n == 1 { format!("1 {singular}") } else { format!("{n} {plural}") }
}

pub fn prompt(message: &str) -> Result<String> {
	print!("{message}");
	io::stdout().flush().context("Could not write to stdout")?;
	let mut buf = String::new();
	io::stdin().read_line(&mut buf).context("Could not read from stdin")?;
	Ok(buf)
}
