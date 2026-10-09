//! T077: build integration workspaces from exact accepted artifact commits.
//!
//! An [`IntegrationPlan`] names a base commit plus an ordered list of
//! accepted artifact commits (one per worker workspace). [`build_integration_workspace`]
//! creates a fresh worktree pinned to the base commit, merges each parent in
//! order with `--no-ff` so every parent revision is recorded in history, and
//! returns the merge commit plus the full parent SHA list.
//!
//! Failure discipline: every parent commit is verified to exist before any
//! worktree is created. If a merge conflicts, the merge is aborted and the
//! freshly created worktree is removed (it contains no worker data — only
//! our own aborted merge), and a [`FabricError::Conflict`] is returned. No
//! other destructive git operation is ever emitted.

use crate::hash::{hash_bytes, ContentHash};
use crate::workspace::WorkspaceConfig;
use fabric_types::{FabricError, Result, TaskId};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// One accepted artifact: the exact commit a worker's work was accepted at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedArtifact {
    pub workspace_id: String,
    pub task_id: TaskId,
    /// Commit SHA as accepted (short SHAs are resolved to full SHAs at build).
    pub commit: String,
}

/// Ordered plan for building an integration workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationPlan {
    pub integration_id: String,
    pub base_commit: String,
    /// Parents in merge order.
    pub parents: Vec<AcceptedArtifact>,
}

impl IntegrationPlan {
    /// Canonical digest of the plan (sha256 over canonical JSON).
    pub fn digest(&self) -> Result<ContentHash> {
        let json = serde_json::to_string(self).map_err(|e| FabricError::InvalidInput {
            field: "integration_plan",
            reason: format!("not serializable: {e}"),
        })?;
        Ok(hash_bytes(json.as_bytes()))
    }
}

/// The built integration workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationWorkspace {
    pub path: PathBuf,
    pub branch: String,
    pub base_commit: String,
    /// HEAD after all parents merged.
    pub merge_commit: String,
    /// Full SHAs of every parent, in merge order.
    pub parent_commits: Vec<String>,
    /// Digest of the plan that produced this workspace.
    pub plan_digest: String,
}

fn run_git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| FabricError::TransportFailure(format!("cannot execute git: {e}")))?;
    if !out.status.success() {
        return Err(FabricError::InvalidInput {
            field: "git",
            reason: format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn commit_exists(repo: &Path, commit: &str) -> bool {
    run_git(repo, &["cat-file", "-e", commit]).is_ok()
}

/// Build the integration workspace described by `plan`.
///
/// Contract:
/// - All commits (base + parents) are verified to exist first; nothing is
///   created until they do.
/// - The worktree is created at `worktree_root/<integration_id>` on a fresh
///   branch `<branch_prefix><integration_id>` pinned to `base_commit`.
/// - Parents merge in plan order with `--no-ff`; the returned
///   `parent_commits` are full SHAs in that order.
/// - On merge conflict: abort, remove the fresh worktree, return Conflict.
pub fn build_integration_workspace(
    config: &WorkspaceConfig,
    plan: &IntegrationPlan,
) -> Result<IntegrationWorkspace> {
    if plan.integration_id.trim().is_empty() {
        return Err(FabricError::InvalidInput {
            field: "integration_id",
            reason: "must be non-empty".to_string(),
        });
    }
    if plan.parents.is_empty() {
        return Err(FabricError::InvalidInput {
            field: "parents",
            reason: "at least one accepted artifact is required".to_string(),
        });
    }
    let repo = &config.repo_path;

    if !commit_exists(repo, &plan.base_commit) {
        return Err(FabricError::InvalidInput {
            field: "base_commit",
            reason: format!("commit not found: {}", plan.base_commit),
        });
    }
    let mut parent_shas = Vec::with_capacity(plan.parents.len());
    for p in &plan.parents {
        if !commit_exists(repo, &p.commit) {
            return Err(FabricError::InvalidInput {
                field: "parents[].commit",
                reason: format!(
                    "accepted artifact commit not found: {} (workspace {})",
                    p.commit, p.workspace_id
                ),
            });
        }
        parent_shas.push(run_git(repo, &["rev-parse", &p.commit])?);
    }
    let base_sha = run_git(repo, &["rev-parse", &plan.base_commit])?;

    let branch = format!("{}{}", config.branch_prefix, plan.integration_id);
    if run_git(repo, &["rev-parse", "--verify", &format!("refs/heads/{branch}")]).is_ok() {
        return Err(FabricError::Conflict(format!(
            "branch already exists: {branch}"
        )));
    }
    let path = config.worktree_root.join(&plan.integration_id);
    if path.exists() {
        return Err(FabricError::Conflict(format!(
            "worktree path already exists: {}",
            path.display()
        )));
    }
    std::fs::create_dir_all(&config.worktree_root).map_err(|e| {
        FabricError::InvalidInput {
            field: "worktree_root",
            reason: format!("cannot create {}: {e}", config.worktree_root.display()),
        }
    })?;

    // Worktree creation happens after all commits are verified; if it fails
    // here there is nothing to clean up yet.
    run_git(
        repo,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            path.to_str().ok_or(FabricError::InvalidInput {
                field: "worktree_root",
                reason: "path is not valid UTF-8".to_string(),
            })?,
            &base_sha,
        ],
    )?;

    // Merge parents in order. Any failure -> abort + remove the fresh worktree.
    let merge_result: Result<()> = (|| {
        for (sha, parent) in parent_shas.iter().zip(&plan.parents) {
            let msg = format!("integrate {} ({})", sha, parent.workspace_id);
            let r = run_git(
                &path,
                &[
                    "-c",
                    "user.name=fabric-integrator",
                    "-c",
                    "user.email=fabric@local",
                    "merge",
                    "--no-ff",
                    "--no-edit",
                    "-m",
                    &msg,
                    sha,
                ],
            );
            if let Err(e) = r {
                let _ = run_git(&path, &["merge", "--abort"]);
                return Err(FabricError::Conflict(format!(
                    "merge conflict integrating {} (workspace {}): {e}",
                    sha, parent.workspace_id
                )));
            }
        }
        Ok(())
    })();

    if let Err(e) = merge_result {
        // The worktree holds only our own aborted merge; safe to remove.
        let _ = run_git(repo, &["worktree", "remove", &path.to_string_lossy()]);
        return Err(e);
    }

    let merge_commit = run_git(&path, &["rev-parse", "HEAD"])?;
    let plan_digest = plan.digest()?;

    Ok(IntegrationWorkspace {
        path,
        branch,
        base_commit: base_sha,
        merge_commit,
        parent_commits: parent_shas,
        plan_digest: plan_digest.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    struct Repo {
        dir: PathBuf,
    }

    impl Repo {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fabric-ws-int-{}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst),
                tag
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let r = Self { dir };
            r.git(&["init", "-q"]);
            r.git(&[
                "-c", "user.name=t", "-c", "user.email=t@t",
                "commit", "-q", "--allow-empty", "-m", "base",
            ]);
            r
        }
        fn git(&self, args: &[&str]) -> String {
            let o = Command::new("git")
                .arg("-C")
                .arg(&self.dir)
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
        fn commit_file(&self, name: &str, content: &str, msg: &str) -> String {
            if let Some(p) = Path::new(name).parent() {
                if !p.as_os_str().is_empty() {
                    std::fs::create_dir_all(self.dir.join(p)).unwrap();
                }
            }
            std::fs::write(self.dir.join(name), content).unwrap();
            self.git(&["add", "."]);
            self.git(&[
                "-c", "user.name=t", "-c", "user.email=t@t",
                "commit", "-q", "-m", msg,
            ]);
            self.git(&["rev-parse", "HEAD"])
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn config(r: &Repo, tag: &str) -> WorkspaceConfig {
        WorkspaceConfig {
            repo_path: r.dir.clone(),
            worktree_root: r.dir.join(format!("wt-{tag}")),
            branch_prefix: "integ-test/".to_string(),
        }
    }

    fn artifact(ws: &str, tid: &str, commit: &str) -> AcceptedArtifact {
        AcceptedArtifact {
            workspace_id: ws.to_string(),
            task_id: tid.parse().unwrap(),
            commit: commit.to_string(),
        }
    }

    #[test]
    fn merges_two_disjoint_artifacts_and_records_parents() {
        let r = Repo::new("ok");
        let base = r.git(&["rev-parse", "HEAD"]);
        // worker A on a.rs
        r.git(&["checkout", "-q", "-b", "w-a"]);
        let sha_a = r.commit_file("a.rs", "a", "worker a");
        // worker B on b.rs, from base
        r.git(&["checkout", "-q", &base]);
        r.git(&["checkout", "-q", "-b", "w-b"]);
        let sha_b = r.commit_file("b.rs", "b", "worker b");
        r.git(&["checkout", "-q", &base]);

        let cfg = config(&r, "ok");
        let plan = IntegrationPlan {
            integration_id: "i1".to_string(),
            base_commit: base.clone(),
            parents: vec![artifact("w-a", "T071", &sha_a), artifact("w-b", "T072", &sha_b)],
        };
        let iw = build_integration_workspace(&cfg, &plan).unwrap();
        assert_eq!(iw.parent_commits, vec![sha_a, sha_b]);
        assert_eq!(iw.base_commit, base);
        assert!(iw.path.join("a.rs").exists());
        assert!(iw.path.join("b.rs").exists());
        assert!(!iw.merge_commit.is_empty());
        assert!(iw.plan_digest.starts_with("sha256:"));
        // plan digest is stable
        assert_eq!(plan.digest().unwrap().to_string(), iw.plan_digest);
        // cleanup the integration worktree (test-owned)
        run_git(&r.dir, &["worktree", "remove", &iw.path.to_string_lossy()]).unwrap();
    }

    #[test]
    fn rejects_unknown_parent_commit_before_creating_anything() {
        let r = Repo::new("badparent");
        let base = r.git(&["rev-parse", "HEAD"]);
        let cfg = config(&r, "badparent");
        let plan = IntegrationPlan {
            integration_id: "i-bad".to_string(),
            base_commit: base,
            parents: vec![artifact("w-x", "T071", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef")],
        };
        let err = build_integration_workspace(&cfg, &plan).unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }), "{err:?}");
        assert!(!cfg.worktree_root.join("i-bad").exists());
    }

    #[test]
    fn merge_conflict_aborts_and_removes_worktree() {
        let r = Repo::new("conflict");
        let base = r.git(&["rev-parse", "HEAD"]);
        r.git(&["checkout", "-q", "-b", "w-a"]);
        let sha_a = r.commit_file("same.rs", "from-a", "a");
        r.git(&["checkout", "-q", &base]);
        r.git(&["checkout", "-q", "-b", "w-b"]);
        let sha_b = r.commit_file("same.rs", "from-b", "b");
        r.git(&["checkout", "-q", &base]);

        let cfg = config(&r, "conflict");
        let plan = IntegrationPlan {
            integration_id: "i-conf".to_string(),
            base_commit: base,
            parents: vec![artifact("w-a", "T071", &sha_a), artifact("w-b", "T072", &sha_b)],
        };
        let err = build_integration_workspace(&cfg, &plan).unwrap_err();
        assert!(matches!(err, FabricError::Conflict(_)), "{err:?}");
        assert!(err.to_string().contains("merge conflict"));
        // fresh worktree was removed; no residue
        assert!(!cfg.worktree_root.join("i-conf").exists());
        let list = r.git(&["worktree", "list", "--porcelain"]);
        assert!(!list.contains("i-conf"), "{list}");
    }

    #[test]
    fn rejects_empty_id_and_empty_parents() {
        let r = Repo::new("empty");
        let base = r.git(&["rev-parse", "HEAD"]);
        let cfg = config(&r, "empty");
        let p1 = IntegrationPlan {
            integration_id: "".to_string(),
            base_commit: base.clone(),
            parents: vec![artifact("w", "T071", &base)],
        };
        assert!(build_integration_workspace(&cfg, &p1).is_err());
        let p2 = IntegrationPlan {
            integration_id: "i".to_string(),
            base_commit: base,
            parents: vec![],
        };
        assert!(build_integration_workspace(&cfg, &p2).is_err());
    }
}
