//! Workspace and Git isolation (T071-T078).
//!
//! Isolated per-task Git worktrees pinned to a base commit, scope
//! enforcement, attempt metadata, normalized diff capture, patch hashing,
//! overlap detection, integration workspace assembly, and policy-gated
//! cleanup.
//!
//! Interface contract frozen by the orchestrator (PHASE 2): `workspace`.
//! Workers implement bodies and own the remaining modules outright.
//!
//! Destructive Git operations (reset/stash/clean -f/force-push/history
//! rewriting) are NEVER authorized for workers; see the lane brief template
//! precedent. This crate shells out to the `git` CLI via std::process.

pub mod cleanup;
pub mod diff;
pub mod hash;
pub mod integrate;
pub mod meta;
pub mod overlap;
pub mod scope;
pub mod workspace;

pub use workspace::{Workspace, WorkspaceConfig};
