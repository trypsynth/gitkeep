use std::time::Duration;

use anyhow::Result;
use chrono::{Duration as ChronoDuration, Utc};
use tokio::time;

pub async fn run(interval_secs: u64) -> Result<()> {
	println!("Watch mode started. Syncing every {interval_secs} seconds.");
	println!("Press Ctrl+C to stop.");
	loop {
		if let Err(e) = crate::sync::run(&[], false).await {
			eprintln!("Sync error: {e:#}.");
		}
		let delta = ChronoDuration::try_seconds(interval_secs as i64).unwrap_or(ChronoDuration::hours(1));
		let next = Utc::now() + delta;
		println!("Next sync at: {}.", next.format("%Y-%m-%d %H:%M:%S UTC"));
		time::sleep(Duration::from_secs(interval_secs)).await;
	}
}
