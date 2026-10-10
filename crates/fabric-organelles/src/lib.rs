//! Organelle registry (REG-01..).
//!
//! Registration, lookup, and composition planning over Zack's organelle-core
//! model (`Organelle` trait, membrane validation, fractal `Composed`).
//! This crate does NOT reinvent the organelle — it registers and composes
//! the existing one.
//!
//! Interface contract frozen by the orchestrator: `registry`.
//! Workers implement bodies and own the remaining modules outright.

pub mod compose;
pub mod persist;
pub mod record;
pub mod registry;

pub use registry::{OrganelleRegistry, RegistryError};
pub use record::OrganelleRecord;
