//! Run the self-hosting driver on a manifest.
//!
//! Usage: cargo run --offline -p fabric-driver --example run -- <manifest.json> <work_root>
//!
//! The driver writes envelopes to <work_root>/outbox and waits for receipts
//! in <work_root>/inbox. An external worker (this dogfood: the operator)
//! watches the outbox, does the work, and drops `<digest>.receipt.json`
//! files (each embedding its `envelope_digest`) into the inbox.

use fabric_driver::{Driver, DriverConfig};
use std::path::PathBuf;

fn main() {
    let manifest = std::env::args().nth(1).expect("usage: run <manifest> <work_root>");
    let work_root = std::env::args().nth(2).expect("usage: run <manifest> <work_root>");
    let config = DriverConfig {
        manifest_path: PathBuf::from(manifest),
        work_root: PathBuf::from(&work_root),
        poll_interval_ms: 500,
        max_waves: 10,
        wave_timeout_ms: 120_000,
    };
    let driver = Driver::new(config).expect("valid config");
    match driver.run() {
        Ok(report) => {
            println!("{}", serde_json::to_string_pretty(&report).unwrap());
            let complete = matches!(
                report.outcome,
                fabric_driver::WaveOutcome::Complete
            );
            std::process::exit(if complete { 0 } else { 2 });
        }
        Err(e) => {
            eprintln!("driver error: {e}");
            std::process::exit(1);
        }
    }
}
