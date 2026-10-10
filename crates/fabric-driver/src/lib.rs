//! Self-hosting build driver (DRV-01..).
//!
//! The work-order loop: load manifest → build task graph → plan waves →
//! create workspaces → write dispatch envelopes to the outbox (dead drop) →
//! wait for receipts in the inbox → verify → record → integrate. The driver
//! never executes AI work itself and never knows the project; workers never
//! see the DAG. Handoffs occur by digest through the dead drop, per the
//! compartmentalization doctrine.
//!
//! Interface contract frozen by the orchestrator: `driver`.
//! Workers implement bodies and own the remaining modules outright.

pub mod backend;
pub mod config;
pub mod driver;

pub use config::DriverConfig;
pub use driver::{Driver, DriverReport, WaveOutcome};
