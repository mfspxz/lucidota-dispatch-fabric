//! Run the self-hosting driver on a manifest.
//!
//! Usage: cargo run --offline -p fabric-driver --example run -- <manifest.json> <work_root> [verifiers.json] [--allow-degraded]
//!
//! The driver writes envelopes to <work_root>/outbox and waits for receipts
//! in <work_root>/inbox. An external worker (this dogfood: the operator)
//! watches the outbox, does the work, and drops `<digest>.receipt.json`
//! files (each embedding its `envelope_digest`) into the inbox.
//!
//! Without `verifiers.json` the driver refuses to start unless
//! `--allow-degraded` is passed: degraded mode gates tasks on unverified
//! receipt claims, so it requires an explicit opt-in.

use fabric_driver::{Driver, DriverConfig};
use std::path::PathBuf;

fn main() {
    let manifest = std::env::args().nth(1).expect("usage: run <manifest> <work_root> [verifiers.json] [--allow-degraded]");
    let work_root = std::env::args().nth(2).expect("usage: run <manifest> <work_root> [verifiers.json] [--allow-degraded]");
    // The third arg is the verifiers file unless it is the flag.
    let third = std::env::args().nth(3);
    let verifiers_path = third.as_deref().filter(|s| *s != "--allow-degraded").map(PathBuf::from);
    let allow_degraded = std::env::args().any(|a| a == "--allow-degraded");
    let config = DriverConfig {
        manifest_path: PathBuf::from(manifest),
        work_root: PathBuf::from(&work_root),
        max_waves: 10,
        wave_timeout_ms: 120_000,
        // NOTE (cell DRV2, out-of-scope file): the interop scaffold added
        // `verifiers_path` to DriverConfig without updating this example,
        // breaking the package build. Minimal repair: optional third arg.
        verifiers_path,
        // FIX-4: degraded mode (no verifiers) requires explicit opt-in.
        allow_degraded,
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
