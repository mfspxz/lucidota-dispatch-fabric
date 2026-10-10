//! T071 — INTERFACE CONTRACT (frozen by orchestrator).
//!
//! Create isolated per-task Git worktrees (or equivalent workspaces) pinned
//! to a base commit.
//!
//! Worker W1: replace the `todo!()` bodies with production implementations.
//! Public signatures are frozen; add private helpers freely. Shell out to
//! the `git` CLI; never implement Git plumbing by hand.

use fabric_types::{FabricError, Result, TaskId};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

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
    /// Base commit the worktree was pinned to (full SHA).
    pub base_commit: String,
    pub task_id: TaskId,
    pub attempt: u32,
}

/// Run the git CLI in `dir`. Never touches the caller's checkout beyond the
/// exact subcommand issued.
fn run_git(dir: &Path, args: &[&str]) -> Result<std::process::Output> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| FabricError::TransportFailure(format!("failed to spawn git: {e}")))?;
    Ok(out)
}

/// Truncated stderr for error messages (never unbounded).
fn stderr_of(out: &std::process::Output) -> String {
    let s = String::from_utf8_lossy(&out.stderr);
    let s = s.trim();
    if s.len() > 500 {
        format!("{}…", &s[..500])
    } else {
        s.to_string()
    }
}

/// `n` random bytes from the OS, hex-encoded. Fail-closed when the OS
/// gives no randomness — an unpredictable workspace ID must not silently
/// degrade to a predictable one (DS2-003).
fn random_hex(n: usize) -> Result<String> {
    use std::io::Read;
    let mut buf = vec![0u8; n];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .map_err(|e| FabricError::TransportFailure(format!("OS randomness unavailable: {e}")))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
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
    if base_commit.trim().is_empty() {
        return Err(FabricError::InvalidInput {
            field: "base_commit",
            reason: "must be non-empty".to_string(),
        });
    }
    if !config.repo_path.is_dir() {
        return Err(FabricError::InvalidInput {
            field: "repo_path",
            reason: "must be an existing directory".to_string(),
        });
    }
    if !config.worktree_root.is_dir() {
        return Err(FabricError::InvalidInput {
            field: "worktree_root",
            reason: "must be an existing directory".to_string(),
        });
    }

    // The repo must actually be a git repository.
    let probe = run_git(&config.repo_path, &["rev-parse", "--git-dir"])?;
    if !probe.status.success() {
        return Err(FabricError::InvalidInput {
            field: "repo_path",
            reason: "not a git repository".to_string(),
        });
    }

    // Pin: resolve the base to a full commit SHA, reject anything else.
    let resolved = run_git(
        &config.repo_path,
        &["rev-parse", "--verify", &format!("{base_commit}^{{commit}}")],
    )?;
    if !resolved.status.success() {
        return Err(FabricError::InvalidInput {
            field: "base_commit",
            reason: format!("does not resolve to a commit: {}", stderr_of(&resolved)),
        });
    }
    let base_sha = String::from_utf8_lossy(&resolved.stdout).trim().to_string();

    // DS2-003: the workspace ID carries a random suffix. A bare
    // `{task}-a{attempt}` ID is predictable: a wave-1 worker can pre-create
    // the next attempt's worktree path (or branch) and turn the
    // fail-on-exists below into a run-killing DoS with no report. The
    // `{task}-a{attempt}` prefix stays human-readable and deterministic;
    // the suffix makes the full ID unpredictable. `workspace_id()` itself
    // is unchanged (frozen interface); uniqueness is a `create_workspace`
    // property. The branch-exists / path-exists checks below remain as
    // defense in depth.
    let id = format!(
        "{}-{}",
        Workspace::workspace_id(task_id, attempt),
        random_hex(8)?
    );
    // TaskId charset permits '.', so guard against path traversal explicitly.
    if id.contains("..") || id.contains('/') || id.contains('\\') {
        return Err(FabricError::InvalidInput {
            field: "task_id",
            reason: "workspace ID must not contain path separators".to_string(),
        });
    }
    let branch = format!("{}{}", config.branch_prefix, id);

    // Fresh branch required: refuse if it already exists.
    let exists = run_git(
        &config.repo_path,
        &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")],
    )?;
    if exists.status.success() {
        return Err(FabricError::Conflict(format!(
            "branch {branch} already exists"
        )));
    }
    // Validate the branch name is well-formed before use.
    let fmt = run_git(&config.repo_path, &["check-ref-format", "--branch", &branch])?;
    if !fmt.status.success() {
        return Err(FabricError::InvalidInput {
            field: "branch",
            reason: format!("invalid branch name: {}", stderr_of(&fmt)),
        });
    }

    let path = config.worktree_root.join(&id);
    if path.exists() {
        return Err(FabricError::Conflict(format!(
            "worktree path {} already exists",
            path.display()
        )));
    }

    // Create the worktree on a fresh branch from the pinned commit.
    // This does not alter the main checkout's branch or index.
    let add = run_git(
        &config.repo_path,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            path.to_str().ok_or(FabricError::InvalidInput {
                field: "worktree_root",
                reason: "worktree path is not valid UTF-8".to_string(),
            })?,
            &base_sha,
        ],
    )?;
    if !add.status.success() {
        return Err(FabricError::TransportFailure(format!(
            "git worktree add failed: {}",
            stderr_of(&add)
        )));
    }

    Ok(Workspace {
        id,
        path,
        branch,
        base_commit: base_sha,
        task_id: task_id.clone(),
        attempt,
    })
}

impl Workspace {
    /// Workspace ID: `{task_id}-a{attempt}`. Stable and unique per attempt.
    pub fn workspace_id(task_id: &TaskId, attempt: u32) -> String {
        format!("{}-a{}", task_id.as_str(), attempt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Throwaway git repo under the OS temp dir. Never touches the real repo.
    fn temp_repo() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "w1-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", stderr_of(&out));
            out
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "w1@test"]);
        git(&["config", "user.name", "w1"]);
        git(&["config", "commit.gpgsign", "false"]);
        fs::write(dir.join("seed.txt"), "seed\n").unwrap();
        git(&["add", "seed.txt"]);
        git(&["commit", "-q", "-m", "seed"]);
        dir
    }

    fn base_sha(repo: &Path) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn config_for(repo: &Path) -> (WorkspaceConfig, PathBuf) {
        let root = repo.join("worktrees");
        fs::create_dir_all(&root).unwrap();
        (
            WorkspaceConfig {
                repo_path: repo.to_path_buf(),
                worktree_root: root.clone(),
                branch_prefix: "w1/".to_string(),
            },
            root,
        )
    }

    /// Remove worktrees created during a test, then drop the temp repo.
    fn teardown(repo: &Path) {
        // Remove each registered worktree path, then prune.
        let list = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&list.stdout);
        for line in text.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                let p = Path::new(p);
                if p != repo {
                    let _ = Command::new("git")
                        .arg("-C")
                        .arg(repo)
                        .args(["worktree", "remove", &p.to_string_lossy()])
                        .output();
                    let _ = fs::remove_dir_all(p);
                }
            }
        }
        let _ = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["worktree", "prune"])
            .output();
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn creates_pinned_worktree_on_fresh_branch() {
        let repo = temp_repo();
        let (cfg, _root) = config_for(&repo);
        let sha = base_sha(&repo);
        let task = TaskId::new("T071").unwrap();

        let ws = create_workspace(&cfg, &sha, &task, 1).unwrap();
        assert_eq!(ws.id, "T071-a1");
        assert_eq!(ws.branch, "w1/T071-a1");
        assert_eq!(ws.base_commit, sha);
        assert!(ws.path.is_dir());
        assert!(ws.path.join("seed.txt").is_file());

        // Worktree HEAD is exactly the pinned commit.
        let head = base_sha(&ws.path);
        assert_eq!(head, sha);

        // Main checkout untouched: still on main.
        let out = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "main");

        teardown(&repo);
    }

    #[test]
    fn same_task_attempt_gets_unique_ids() {
        // DS2-003: workspace IDs are unpredictable per creation. Two
        // workspaces for the same (task, attempt) must not collide — the
        // old deterministic ID let a worker pre-create the next attempt's
        // path/branch and DoS the run. Both creations succeed with
        // distinct IDs sharing the deterministic `{task}-a{attempt}`
        // prefix. The branch-exists / path-exists guards remain as
        // defense in depth against a true random collision.
        let repo = temp_repo();
        let (cfg, _root) = config_for(&repo);
        let sha = base_sha(&repo);
        let task = TaskId::new("T071").unwrap();

        let w1 = create_workspace(&cfg, &sha, &task, 1).unwrap();
        let w2 = create_workspace(&cfg, &sha, &task, 1).unwrap();
        assert_ne!(w1.id, w2.id, "workspace IDs must be unique");
        assert_ne!(w1.path, w2.path, "workspace paths must be unique");
        // Deterministic, human-readable prefix; unpredictable suffix.
        for w in [&w1, &w2] {
            assert!(
                w.id.starts_with(&format!("{}-a1-", task.as_str())),
                "unexpected id {}",
                w.id
            );
            let suffix = w.id.rsplit('-').next().unwrap();
            assert_eq!(suffix.len(), 16, "suffix must be 16 hex chars");
            assert!(
                suffix.chars().all(|c| c.is_ascii_hexdigit()),
                "suffix must be hex: {suffix}"
            );
        }
        teardown(&repo);
    }

    #[test]
    fn rejects_unresolvable_base_commit() {
        let repo = temp_repo();
        let (cfg, _root) = config_for(&repo);
        let task = TaskId::new("T071").unwrap();
        let err = create_workspace(&cfg, "deadbeef", &task, 1).unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }), "{err:?}");
        teardown(&repo);
    }

    #[test]
    fn rejects_non_repo_path() {
        let repo = temp_repo();
        let (mut cfg, _root) = config_for(&repo);
        cfg.repo_path = std::env::temp_dir(); // not a git repo
        let task = TaskId::new("T071").unwrap();
        let err = create_workspace(&cfg, "HEAD", &task, 1).unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }), "{err:?}");
        teardown(&repo);
    }

    #[test]
    fn workspace_id_format_is_stable() {
        let t = TaskId::new("T118").unwrap();
        assert_eq!(Workspace::workspace_id(&t, 3), "T118-a3");
    }
}
