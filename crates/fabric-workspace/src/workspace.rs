//! T071 — INTERFACE CONTRACT (frozen by orchestrator).
//!
//! Create isolated per-task Git worktrees (or equivalent workspaces) pinned
//! to a base commit.
//!
//! Worker W1: replace the `todo!()` bodies with production implementations.
//! Public signatures are frozen; add private helpers freely. Shell out to
//! the `git` CLI; never implement Git plumbing by hand.

use fabric_types::{Result, TaskId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Configuration for creating a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    /// Repository the worktree is created from.
    pub repo_path: PathBuf,
    /// Directory under which worktrees are created.
    pub worktree_root: PathBuf,
    /// Branch name prefix for worker branches.
    pub branch_prefix: String,
}

/// An isolated per-task workspace pinned to a base commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    /// Stable workspace ID (derived from task ID + attempt).
    pub id: String,
    /// Filesystem path of the worktree.
    pub path: PathBuf,
    /// Branch the worker commits on.
    pub branch: String,
    /// Base commit the worktree was pinned to.
    pub base_commit: String,
    pub task_id: TaskId,
    pub attempt: u32,
}

/// Create an isolated worktree for one task attempt, pinned to `base_commit`.
///
/// Contract:
/// - Uses `git worktree add`; the branch is created fresh and must not exist.
/// - Never touches the main checkout's branch or index.
/// - Returns the workspace handle; cleanup is T078's job, not this function's.
pub fn create_workspace(
    config: &WorkspaceConfig,
    base_commit: &str,
    task_id: &TaskId,
    attempt: u32,
) -> Result<Workspace> {
    let _ = (config, base_commit, task_id, attempt);
    todo!("T071: implement worktree creation")
}

impl Workspace {
    /// Workspace ID: `{task_id}-a{attempt}`. Stable and unique per attempt.
    pub fn workspace_id(task_id: &TaskId, attempt: u32) -> String {
        format!("{}-a{}", task_id.as_str(), attempt)
    }
}
