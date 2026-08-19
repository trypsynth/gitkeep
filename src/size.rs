use std::{fmt::Write as _, fs, path::Path};

use anyhow::{Context, Result};
use clap::ValueEnum;

use crate::config::Config;

#[derive(Clone, Copy, ValueEnum)]
pub enum SizeFormat {
	/// Powers of 1000 (kB, MB, GB, ...)
	Decimal,
	/// Powers of 1024 (KiB, MiB, GiB, ...)
	Binary,
	/// Exact byte count, no unit conversion
	Raw,
}

pub fn run(format: SizeFormat) -> Result<()> {
	let config = Config::load()?;
	let archive_dir = config.archive_dir()?;
	if !archive_dir.exists() {
		println!("Archive directory {} does not exist yet.", archive_dir.display());
		return Ok(());
	}
	let mut entries: Vec<(String, u64)> = Vec::new();
	let mut total = 0u64;
	for entry in fs::read_dir(&archive_dir)
		.with_context(|| format!("Could not read archive directory {}", archive_dir.display()))?
	{
		let entry = entry?;
		if !entry.file_type()?.is_dir() {
			continue;
		}
		let size = dir_size(&entry.path())?;
		total += size;
		entries.push((entry.file_name().to_string_lossy().into_owned(), size));
	}
	if entries.is_empty() {
		println!("Archive is empty.");
		return Ok(());
	}
	entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
	let width = entries.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
	let mut out = String::new();
	for (name, size) in &entries {
		let _ = writeln!(out, "  {name:width$}  {}", format_size(*size, format));
	}
	let _ = writeln!(out);
	let _ = write!(out, "Total: {}", format_size(total, format));
	println!("{out}");
	Ok(())
}

/// Recursively sums the apparent size of every regular file under `path`. Symlinks are not
/// followed (their target size isn't counted), which also avoids infinite loops on symlink
/// cycles.
fn dir_size(path: &Path) -> Result<u64> {
	let mut total = 0u64;
	for entry in fs::read_dir(path).with_context(|| format!("Could not read directory {}", path.display()))? {
		let entry = entry?;
		let file_type = entry.file_type()?;
		if file_type.is_dir() {
			total += dir_size(&entry.path())?;
		} else if file_type.is_file() {
			total += entry.metadata()?.len();
		}
	}
	Ok(total)
}

fn format_size(bytes: u64, format: SizeFormat) -> String {
	match format {
		SizeFormat::Raw => format!("{bytes} bytes"),
		SizeFormat::Decimal => format_with_units(bytes, 1000.0, &["B", "kB", "MB", "GB", "TB", "PB"]),
		SizeFormat::Binary => format_with_units(bytes, 1024.0, &["B", "KiB", "MiB", "GiB", "TiB", "PiB"]),
	}
}

fn format_with_units(bytes: u64, base: f64, units: &[&str]) -> String {
	#[allow(clippy::cast_precision_loss)]
	let mut value = bytes as f64;
	let mut unit = 0;
	while value >= base && unit < units.len() - 1 {
		value /= base;
		unit += 1;
	}
	if unit == 0 { format!("{value:.0} {}", units[unit]) } else { format!("{value:.2} {}", units[unit]) }
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn raw_format_shows_exact_bytes() {
		assert_eq!(format_size(12345, SizeFormat::Raw), "12345 bytes");
	}

	#[test]
	fn decimal_format_uses_1000_base() {
		assert_eq!(format_size(1_000_000, SizeFormat::Decimal), "1.00 MB");
	}

	#[test]
	fn binary_format_uses_1024_base() {
		assert_eq!(format_size(1024 * 1024, SizeFormat::Binary), "1.00 MiB");
	}

	#[test]
	fn small_sizes_stay_in_bytes() {
		assert_eq!(format_size(512, SizeFormat::Decimal), "512 B");
		assert_eq!(format_size(512, SizeFormat::Binary), "512 B");
	}

	#[test]
	fn decimal_and_binary_diverge_for_large_sizes() {
		let bytes = 5 * 1024 * 1024 * 1024;
		assert_eq!(format_size(bytes, SizeFormat::Decimal), "5.37 GB");
		assert_eq!(format_size(bytes, SizeFormat::Binary), "5.00 GiB");
	}

	#[test]
	fn zero_bytes_formats_cleanly() {
		assert_eq!(format_size(0, SizeFormat::Decimal), "0 B");
		assert_eq!(format_size(0, SizeFormat::Raw), "0 bytes");
	}

	#[test]
	fn caps_at_largest_unit() {
		let huge = 3_000_000_000_000_000_000u64;
		assert_eq!(format_size(huge, SizeFormat::Decimal), "3000.00 PB");
	}
}
