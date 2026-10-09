//! T078: remove abandoned worktrees only after preserving required evidence.
//!
//! Archive-then-remove discipline:
//! 1. [`archive_workspace`] copies the workspace's logs, patches, receipts,
//!    and recovery metadata into a content-hashed archive and writes an
//!    [`ArchiveManifest`].
//! 2. [`remove_workspace`] refuses to run unless a complete archive manifest
//!    (all [`CleanupPolicy::required_kinds`]) exists, then issues exactly one
//!    destructive operation: `git worktree remove` — never `--force`, never
//!    reset/stash/clean, never touching any other worktree or branch.

use crate::hash::hash_file;
use crate::workspace::Workspace;
use fabric_types::{FabricError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Evidence kinds that must be preserved before a workspace may be removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ArchiveKind {
    Log,
    Patch,
    Receipt,
    RecoveryMeta,
}

impl ArchiveKind {
    fn dir_name(self) -> &'static str {
        match self {
            Self::Log => "logs",
            Self::Patch => "patches",
            Self::Receipt => "receipts",
            Self::RecoveryMeta => "recovery",
        }
    }

    pub fn all() -> Vec<ArchiveKind> {
        vec![
            ArchiveKind::Log,
            ArchiveKind::Patch,
            ArchiveKind::Receipt,
            ArchiveKind::RecoveryMeta,
        ]
    }
}

/// One archived file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedItem {
    pub kind: ArchiveKind,
    /// File name inside the archive (kind dir + name).
    pub file_name: String,
    /// sha256 of the archived copy.
    pub digest: String,
    pub size_bytes: u64,
}

/// Manifest written alongside the archive; the removal gate reads it back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveManifest {
    pub workspace_id: String,
    pub archived_at_millis: u64,
    pub items: Vec<ArchivedItem>,
}

impl ArchiveManifest {
    /// True when every required kind has at least one archived item.
    pub fn is_complete(&self, required: &[ArchiveKind]) -> bool {
        required
            .iter()
            .all(|k| self.items.iter().any(|i| &i.kind == k))
    }
}

/// Policy for archiving and removing workspaces.
#[derive(Debug, Clone)]
pub struct CleanupPolicy {
    /// Root under which `<workspace_id>/` archives are written.
    pub archive_root: PathBuf,
    /// Kinds that must be present before removal. Defaults to all four.
    pub required_kinds: Vec<ArchiveKind>,
}

impl CleanupPolicy {
    pub fn new(archive_root: PathBuf) -> Self {
        Self {
            archive_root,
            required_kinds: ArchiveKind::all(),
        }
    }
}

/// Evidence inputs to archive for one workspace.
#[derive(Debug, Default)]
pub struct ArchiveInputs {
    pub logs: Vec<PathBuf>,
    pub patches: Vec<PathBuf>,
    pub receipts: Vec<PathBuf>,
    pub recovery_meta: Vec<PathBuf>,
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn archive_dir(policy: &CleanupPolicy, workspace: &Workspace) -> PathBuf {
    policy.archive_root.join(&workspace.id)
}

/// Copy logs, patches, receipts, and recovery metadata into the archive,
/// hashing every file, and write `manifest.json`. Returns the manifest.
pub fn archive_workspace(
    workspace: &Workspace,
    policy: &CleanupPolicy,
    inputs: &ArchiveInputs,
) -> Result<ArchiveManifest> {
    let dest = archive_dir(policy, workspace);
    let grouped: [(ArchiveKind, &Vec<PathBuf>); 4] = [
        (ArchiveKind::Log, &inputs.logs),
        (ArchiveKind::Patch, &inputs.patches),
        (ArchiveKind::Receipt, &inputs.receipts),
        (ArchiveKind::RecoveryMeta, &inputs.recovery_meta),
    ];
    let mut items = Vec::new();
    for (kind, paths) in grouped {
        let kind_dir = dest.join(kind.dir_name());
        std::fs::create_dir_all(&kind_dir).map_err(|e| FabricError::InvalidInput {
            field: "archive_root",
            reason: format!("cannot create {}: {e}", kind_dir.display()),
        })?;
        for src in paths {
            let name = src.file_name().ok_or(FabricError::InvalidInput {
                field: "archive_inputs",
                reason: format!("path has no file name: {}", src.display()),
            })?;
            let target = kind_dir.join(name);
            std::fs::copy(src, &target).map_err(|e| FabricError::InvalidInput {
                field: "archive_inputs",
                reason: format!("cannot copy {}: {e}", src.display()),
            })?;
            let hash = hash_file(&target)?;
            let size = std::fs::metadata(&target)
                .map(|m| m.len())
                .unwrap_or(0);
            items.push(ArchivedItem {
                kind,
                file_name: format!("{}/{}", kind.dir_name(), name.to_string_lossy()),
                digest: hash.to_string(),
                size_bytes: size,
            });
        }
    }
    items.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    let manifest = ArchiveManifest {
        workspace_id: workspace.id.clone(),
        archived_at_millis: now_millis(),
        items,
    };
    let manifest_path = dest.join("manifest.json");
    let json = serde_json::to_string_pretty(&manifest).map_err(|e| {
        FabricError::InvalidInput {
            field: "archive_manifest",
            reason: format!("not serializable: {e}"),
        }
    })?;
    std::fs::write(&manifest_path, json).map_err(|e| FabricError::InvalidInput {
        field: "archive_root",
        reason: format!("cannot write {}: {e}", manifest_path.display()),
    })?;
    Ok(manifest)
}

fn read_manifest(policy: &CleanupPolicy, workspace: &Workspace) -> Result<ArchiveManifest> {
    let p = archive_dir(policy, workspace).join("manifest.json");
    let data = std::fs::read(&p).map_err(|_| {
        FabricError::PolicyDenied(format!(
            "refusing to remove workspace {}: no archive manifest at {}",
            workspace.id,
            p.display()
        ))
    })?;
    serde_json::from_slice(&data).map_err(|e| {
        FabricError::PolicyDenied(format!(
            "refusing to remove workspace {}: unreadable archive manifest: {e}",
            workspace.id
        ))
    })
}

/// Remove a workspace's worktree — only if its archive is complete.
///
/// This issues exactly one destructive git operation (`git worktree remove`,
/// never `--force`). Anything else (missing/incomplete archive) is a
/// [`FabricError::PolicyDenied`].
pub fn remove_workspace(
    repo: &Path,
    workspace: &Workspace,
    policy: &CleanupPolicy,
) -> Result<()> {
    let manifest = read_manifest(policy, workspace)?;
    if !manifest.is_complete(&policy.required_kinds) {
        return Err(FabricError::PolicyDenied(format!(
            "refusing to remove workspace {}: archive incomplete (required kinds missing)",
            workspace.id
        )));
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "remove", &workspace.path.to_string_lossy()])
        .output()
        .map_err(|e| FabricError::TransportFailure(format!("cannot execute git: {e}")))?;
    if !out.status.success() {
        return Err(FabricError::InvalidInput {
            field: "git",
            reason: format!(
                "git worktree remove failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::WorkspaceConfig;
    use fabric_types::TaskId;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "fabric-ws-clean-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst),
            tag
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn git(repo: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    fn workspace(id: &str, path: PathBuf) -> Workspace {
        Workspace {
            id: id.to_string(),
            path,
            branch: format!("wt/{id}"),
            base_commit: "abc".to_string(),
            task_id: TaskId::new("T078").unwrap(),
            attempt: 1,
        }
    }

    fn write_inputs(d: &Path) -> ArchiveInputs {
        let mk = |n: &str, c: &str| {
            let p = d.join(n);
            std::fs::write(&p, c).unwrap();
            p
        };
        ArchiveInputs {
            logs: vec![mk("run.log", "log1")],
            patches: vec![mk("fix.patch", "diff")],
            receipts: vec![mk("T078.json", "{}")],
            recovery_meta: vec![mk("meta.json", "{}")],
        }
    }

    #[test]
    fn archive_writes_manifest_with_hashes() {
        let d = tmp("archive");
        let ws = workspace("w-1", d.join("wt"));
        let policy = CleanupPolicy::new(d.join("archives"));
        let m = archive_workspace(&ws, &policy, &write_inputs(&d)).unwrap();
        assert_eq!(m.items.len(), 4);
        assert!(m.is_complete(&ArchiveKind::all()));
        assert!(d.join("archives/w-1/manifest.json").exists());
        assert!(d.join("archives/w-1/logs/run.log").exists());
        assert!(m.items.iter().all(|i| i.digest.starts_with("sha256:")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn remove_refuses_without_archive() {
        let d = tmp("noarchive");
        git(&d, &["init", "-q"]);
        let ws = workspace("w-2", d.join("wt"));
        let policy = CleanupPolicy::new(d.join("archives"));
        let err = remove_workspace(&d, &ws, &policy).unwrap_err();
        assert!(matches!(err, FabricError::PolicyDenied(_)), "{err:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn remove_refuses_incomplete_archive() {
        let d = tmp("incomplete");
        git(&d, &["init", "-q"]);
        let ws = workspace("w-3", d.join("wt"));
        let policy = CleanupPolicy::new(d.join("archives"));
        let mut inputs = write_inputs(&d);
        inputs.recovery_meta.clear();
        let m = archive_workspace(&ws, &policy, &inputs).unwrap();
        assert!(!m.is_complete(&ArchiveKind::all()));
        let err = remove_workspace(&d, &ws, &policy).unwrap_err();
        assert!(matches!(err, FabricError::PolicyDenied(_)), "{err:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn remove_after_full_archive_deletes_worktree() {
        let d = tmp("full");
        git(&d, &["init", "-q"]);
        git(
            &d,
            &[
                "-c", "user.name=t", "-c", "user.email=t@t",
                "commit", "-q", "--allow-empty", "-m", "base",
            ],
        );
        let wt_path = d.join("wtree");
        git(&d, &["worktree", "add", "-b", "wt/w-4", wt_path.to_str().unwrap(), "HEAD"]);
        assert!(wt_path.exists());

        let ws = workspace("w-4", wt_path.clone());
        let policy = CleanupPolicy::new(d.join("archives"));
        archive_workspace(&ws, &policy, &write_inputs(&d)).unwrap();
        remove_workspace(&d, &ws, &policy).unwrap();
        assert!(!wt_path.exists());
        let list = git(&d, &["worktree", "list", "--porcelain"]);
        assert!(!list.contains("wtree"), "{list}");
        // archive survives removal
        assert!(d.join("archives/w-4/manifest.json").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn archive_is_complete_check() {
        let m = ArchiveManifest {
            workspace_id: "w".into(),
            archived_at_millis: 0,
            items: vec![ArchivedItem {
                kind: ArchiveKind::Log,
                file_name: "logs/a".into(),
                digest: "sha256:00".into(),
                size_bytes: 1,
            }],
        };
        assert!(m.is_complete(&[ArchiveKind::Log]));
        assert!(!m.is_complete(&ArchiveKind::all()));
    }

    #[test]
    fn config_unused_silence() {
        // WorkspaceConfig is W1's surface; ensure our modules compile against it.
        let c = WorkspaceConfig {
            repo_path: PathBuf::from("/r"),
            worktree_root: PathBuf::from("/w"),
            branch_prefix: "wt/".into(),
        };
        assert_eq!(c.branch_prefix, "wt/");
    }
}
