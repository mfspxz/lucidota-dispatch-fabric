//! T073: record base commit, branch, workspace ID, and spec revision.
//!
//! Every attempt gets an [`AttemptMeta`] tying it to the exact workspace it
//! ran in: workspace ID, task ID, attempt number, pinned base commit, worker
//! branch, spec revision, the worktree's actual HEAD at record time, and a
//! timestamp. [`AttemptMeta::write_json`] persists it as `attempt-meta.json`
//! for the evidence ledger.

use crate::workspace::Workspace;
use fabric_types::{FabricError, Result, TaskId};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Metadata recorded for one task attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptMeta {
    pub workspace_id: String,
    pub task_id: TaskId,
    pub attempt: u32,
    /// Pinned base commit (full SHA) the worktree was created from.
    pub base_commit: String,
    /// Worker branch for this attempt.
    pub branch: String,
    /// Spec revision the task contract was compiled from.
    pub spec_revision: String,
    /// Worktree HEAD when the metadata was recorded, if resolvable.
    pub head_commit: Option<String>,
    /// Seconds since the Unix epoch.
    pub recorded_at_unix: u64,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Resolve the worktree's current HEAD. `None` when unresolvable — the
/// metadata records what can be proven, never invented.
pub fn capture_head(workspace_path: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(workspace_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sha.is_empty() {
        None
    } else {
        Some(sha)
    }
}

impl AttemptMeta {
    /// Record metadata for an attempt running in `workspace`.
    pub fn record(workspace: &Workspace, spec_revision: &str) -> Result<Self> {
        if spec_revision.trim().is_empty() {
            return Err(FabricError::InvalidInput {
                field: "spec_revision",
                reason: "must be non-empty".to_string(),
            });
        }
        Ok(Self {
            workspace_id: workspace.id.clone(),
            task_id: workspace.task_id.clone(),
            attempt: workspace.attempt,
            base_commit: workspace.base_commit.clone(),
            branch: workspace.branch.clone(),
            spec_revision: spec_revision.to_string(),
            head_commit: capture_head(&workspace.path),
            recorded_at_unix: now_unix(),
        })
    }

    /// Persist as `<dir>/attempt-meta.json`. Returns the written path.
    pub fn write_json(&self, dir: &Path) -> Result<PathBuf> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| FabricError::MalformedOutput(format!("meta serialization failed: {e}")))?;
        let path = dir.join("attempt-meta.json");
        std::fs::write(&path, json)
            .map_err(|e| FabricError::TransportFailure(format!("failed to write meta: {e}")))?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fake_workspace() -> (Workspace, PathBuf) {
        let dir = std::env::temp_dir().join(format!("w1-meta-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let ws = Workspace {
            id: "T073-a1".to_string(),
            path: dir.clone(),
            branch: "w1/T073-a1".to_string(),
            base_commit: "abc123".to_string(),
            task_id: TaskId::new("T073").unwrap(),
            attempt: 1,
        };
        (ws, dir)
    }

    #[test]
    fn records_all_required_fields() {
        let (ws, dir) = fake_workspace();
        let m = AttemptMeta::record(&ws, "master-spec v2026-10-09").unwrap();
        assert_eq!(m.workspace_id, "T073-a1");
        assert_eq!(m.base_commit, "abc123");
        assert_eq!(m.branch, "w1/T073-a1");
        assert_eq!(m.spec_revision, "master-spec v2026-10-09");
        assert_eq!(m.task_id.as_str(), "T073");
        assert_eq!(m.attempt, 1);
        assert!(m.recorded_at_unix > 0);
        // Not a git repo: head is honestly None, not invented.
        assert_eq!(m.head_commit, None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_empty_spec_revision() {
        let (ws, dir) = fake_workspace();
        let err = AttemptMeta::record(&ws, "  ").unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_json_round_trips() {
        let (ws, dir) = fake_workspace();
        let m = AttemptMeta::record(&ws, "rev-1").unwrap();
        let path = m.write_json(&dir).unwrap();
        assert_eq!(path.file_name().unwrap(), "attempt-meta.json");
        let back: AttemptMeta =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(m, back);
        let _ = fs::remove_dir_all(&dir);
    }
}
