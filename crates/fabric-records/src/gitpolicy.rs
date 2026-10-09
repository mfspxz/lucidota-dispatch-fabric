//! T079 — Git policy enforcement.
//!
//! Permit local branches and commits under policy; gate pushes, protected
//! merges, tags, and releases. A [`GitPolicy`] holds allowed branch
//! patterns, protected branches requiring review, and per-pattern push
//! rules; [`GitPolicy::check_operation`] returns
//! [`FabricError::PolicyDenied`] for gated operations.
//!
//! This module shells out to the `git` CLI for READ-ONLY inspection only
//! (see [`GitInspector`]). It NEVER emits destructive git operations:
//! no reset, stash, clean -f, force-push, or history rewriting anywhere.

use fabric_types::{FabricError, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

/// A git operation a worker may attempt. Carries the identity of the actor
/// attempting it so push rules can be enforced per identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitOperation {
    /// Create a local branch.
    CreateBranch { branch: String, identity: String },
    /// Commit on a local branch (local only, no push implied).
    Commit { branch: String, identity: String },
    /// Push a branch to a remote.
    Push {
        branch: String,
        remote: String,
        identity: String,
    },
    /// Merge `source` into `target` locally.
    Merge {
        source: String,
        target: String,
        identity: String,
    },
    /// Create a tag (annotated or lightweight — both are gated the same).
    CreateTag { tag: String, identity: String },
    /// Delete a local branch.
    DeleteBranch { branch: String, identity: String },
}

/// Who may push to branches matching a pattern.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushRule {
    /// Branch pattern this rule applies to (see [`pattern_matches`]).
    pub branch_pattern: String,
    /// `None` means nobody may push; `Some(ids)` means only these
    /// identities may push. An empty list is equivalent to `None`.
    pub allowed_identities: Option<Vec<String>>,
}

/// The git policy. Everything not explicitly permitted is denied.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitPolicy {
    /// Branch patterns a worker may create locally (e.g. `wt/*`).
    pub allowed_branch_patterns: Vec<String>,
    /// Branch patterns requiring review: no direct commits, no direct
    /// pushes, no unreviewed merges into them (e.g. `master`, `release/*`).
    pub protected_branches: Vec<String>,
    /// Push rules, evaluated in order; the first matching pattern wins.
    /// No match means the push is denied.
    pub push_rules: Vec<PushRule>,
    /// When true, tag creation is gated (releases flow through review).
    pub tags_require_review: bool,
}

impl GitPolicy {
    /// Check one operation against the policy. `Ok(())` means permitted;
    /// `Err(FabricError::PolicyDenied(..))` carries a machine-readable
    /// reason. Never permits by default: unknown shapes are denied.
    pub fn check_operation(&self, op: &GitOperation) -> Result<()> {
        match op {
            GitOperation::CreateBranch { branch, .. } => {
                if self.is_protected(branch) {
                    return Err(denied(format!(
                        "CREATE_BRANCH_DENIED: branch '{branch}' matches a protected pattern"
                    )));
                }
                if !self.is_allowed_branch(branch) {
                    return Err(denied(format!(
                        "CREATE_BRANCH_DENIED: branch '{branch}' matches no allowed pattern"
                    )));
                }
                Ok(())
            }
            GitOperation::Commit { branch, .. } => {
                // Local commits are permitted on any non-protected branch.
                // Branch creation itself was already gated by policy.
                if self.is_protected(branch) {
                    return Err(denied(format!(
                        "COMMIT_DENIED: branch '{branch}' is protected and requires review"
                    )));
                }
                Ok(())
            }
            GitOperation::Push {
                branch,
                remote,
                identity,
            } => {
                if self.is_protected(branch) {
                    return Err(denied(format!(
                        "PUSH_DENIED: branch '{branch}' is protected and requires review"
                    )));
                }
                match self.push_rules.iter().find(|r| pattern_matches(&r.branch_pattern, branch)) {
                    None => Err(denied(format!(
                        "PUSH_DENIED: no push rule covers branch '{branch}'"
                    ))),
                    Some(rule) => match &rule.allowed_identities {
                        None => Err(denied(format!(
                            "PUSH_DENIED: pushes to '{branch}' are disabled by rule '{}'",
                            rule.branch_pattern
                        ))),
                        Some(ids) if ids.iter().any(|id| id == identity) => Ok(()),
                        Some(_) => Err(denied(format!(
                            "PUSH_DENIED: identity '{identity}' may not push '{branch}' to '{remote}'"
                        ))),
                    },
                }
            }
            GitOperation::Merge { target, .. } => {
                // Merges INTO a protected branch require review: the merge
                // itself is denied here; a reviewed merge is a separate,
                // policy-external approval path, never an override of this
                // check.
                if self.is_protected(target) {
                    return Err(denied(format!(
                        "MERGE_DENIED: target '{target}' is protected and requires review"
                    )));
                }
                Ok(())
            }
            GitOperation::CreateTag { tag, .. } => {
                if self.tags_require_review {
                    return Err(denied(format!(
                        "TAG_DENIED: tag '{tag}' requires review (tags gated by policy)"
                    )));
                }
                Ok(())
            }
            GitOperation::DeleteBranch { branch, .. } => {
                if self.is_protected(branch) {
                    return Err(denied(format!(
                        "DELETE_BRANCH_DENIED: branch '{branch}' is protected"
                    )));
                }
                Ok(())
            }
        }
    }

    fn is_allowed_branch(&self, branch: &str) -> bool {
        self.allowed_branch_patterns
            .iter()
            .any(|p| pattern_matches(p, branch))
    }

    fn is_protected(&self, branch: &str) -> bool {
        self.protected_branches
            .iter()
            .any(|p| pattern_matches(p, branch))
    }
}

fn denied(reason: String) -> FabricError {
    FabricError::PolicyDenied(reason)
}

/// Minimal glob matcher: `*` matches any (possibly empty) sequence of
/// characters. Everything else matches literally. Used for branch patterns
/// such as `wt/*` or `release/*`.
pub fn pattern_matches(pattern: &str, value: &str) -> bool {
    let p = pattern.as_bytes();
    let v = value.as_bytes();
    // Classic two-pointer wildcard match.
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    let mut pi = 0usize;
    let mut vi = 0usize;
    while vi < v.len() {
        if pi < p.len() && (p[pi] == b'*') {
            star = Some(pi);
            mark = vi;
            pi += 1;
        } else if pi < p.len() && p[pi] == v[vi] {
            pi += 1;
            vi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            vi = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// Read-only git inspection. Every command issued is a read: `rev-parse`,
/// `show-ref`. This type has no method that mutates a repository.
pub struct GitInspector;

impl GitInspector {
    /// Resolve HEAD to a full commit SHA. Fails when the repo has no
    /// commits (unborn HEAD).
    pub fn head_commit(repo: &Path) -> Result<String> {
        Self::run(repo, &["rev-parse", "HEAD"])
    }

    /// Current branch name (e.g. `master`). Fails on detached HEAD.
    pub fn current_branch(repo: &Path) -> Result<String> {
        Self::run(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
    }

    /// Whether a local branch exists.
    pub fn branch_exists(repo: &Path, branch: &str) -> Result<bool> {
        reject_option_injection(branch)?;
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .arg("show-ref")
            .arg("--verify")
            .arg("--quiet")
            .arg(format!("refs/heads/{branch}"))
            .output()
            .map_err(|e| FabricError::TransportFailure(format!("git show-ref failed: {e}")))?;
        Ok(out.status.success())
    }

    fn run(repo: &Path, args: &[&str]) -> Result<String> {
        // All argument lists at the call sites are compile-time literals,
        // except the branch name in branch_exists, which is validated by
        // reject_option_injection before it is interpolated.
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .map_err(|e| FabricError::TransportFailure(format!("git {} failed: {e}", args.join(" "))))?;
        if !out.status.success() {
            return Err(FabricError::TransportFailure(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

/// Reject values that could be interpreted as git options (option
/// injection via branch names like `--upload-pack=...`).
fn reject_option_injection(value: &str) -> Result<()> {
    if value.starts_with('-') || value.is_empty() {
        return Err(FabricError::InvalidInput {
            field: "git_argument",
            reason: format!("refusing git argument that looks like an option or is empty: '{value}'"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_policy() -> GitPolicy {
        GitPolicy {
            allowed_branch_patterns: vec!["wt/*".into(), "feature/*".into()],
            protected_branches: vec!["master".into(), "main".into(), "release/*".into()],
            push_rules: vec![
                PushRule {
                    branch_pattern: "wt/*".into(),
                    allowed_identities: Some(vec!["worker-1".into()]),
                },
                PushRule {
                    branch_pattern: "feature/*".into(),
                    allowed_identities: None,
                },
            ],
            tags_require_review: true,
        }
    }

    fn is_denied(r: &Result<()>) -> bool {
        matches!(r, Err(FabricError::PolicyDenied(_)))
    }

    #[test]
    fn pattern_match_basics() {
        assert!(pattern_matches("wt/*", "wt/q1"));
        assert!(pattern_matches("wt/*", "wt/"));
        assert!(!pattern_matches("wt/*", "master"));
        assert!(!pattern_matches("wt/*", "awt/x"));
        assert!(pattern_matches("*", "anything"));
        assert!(pattern_matches("master", "master"));
        assert!(!pattern_matches("master", "master2"));
        assert!(pattern_matches("release/*", "release/2026-10"));
        assert!(!pattern_matches("", "x"));
        // Empty pattern matches only the empty value (standard glob).
        assert!(pattern_matches("", ""));
    }

    #[test]
    fn create_branch_allowed_pattern() {
        let p = test_policy();
        assert!(p
            .check_operation(&GitOperation::CreateBranch {
                branch: "wt/rec1".into(),
                identity: "worker-1".into(),
            })
            .is_ok());
    }

    #[test]
    fn create_branch_denied_no_pattern() {
        let p = test_policy();
        assert!(is_denied(&p.check_operation(&GitOperation::CreateBranch {
            branch: "random".into(),
            identity: "worker-1".into(),
        })));
    }

    #[test]
    fn create_branch_denied_protected() {
        let p = test_policy();
        assert!(is_denied(&p.check_operation(&GitOperation::CreateBranch {
            branch: "master".into(),
            identity: "worker-1".into(),
        })));
    }

    #[test]
    fn commit_allowed_local_denied_prot() {
        let p = test_policy();
        assert!(p
            .check_operation(&GitOperation::Commit {
                branch: "wt/rec1".into(),
                identity: "worker-1".into(),
            })
            .is_ok());
        assert!(is_denied(&p.check_operation(&GitOperation::Commit {
            branch: "master".into(),
            identity: "worker-1".into(),
        })));
    }

    #[test]
    fn push_identity_gated() {
        let p = test_policy();
        // Allowed identity on wt/*.
        assert!(p
            .check_operation(&GitOperation::Push {
                branch: "wt/rec1".into(),
                remote: "origin".into(),
                identity: "worker-1".into(),
            })
            .is_ok());
        // Wrong identity denied.
        assert!(is_denied(&p.check_operation(&GitOperation::Push {
            branch: "wt/rec1".into(),
            remote: "origin".into(),
            identity: "intruder".into(),
        })));
        // feature/* has pushes disabled entirely.
        assert!(is_denied(&p.check_operation(&GitOperation::Push {
            branch: "feature/x".into(),
            remote: "origin".into(),
            identity: "worker-1".into(),
        })));
    }

    #[test]
    fn push_protected_or_unruled_denied() {
        let p = test_policy();
        assert!(is_denied(&p.check_operation(&GitOperation::Push {
            branch: "master".into(),
            remote: "origin".into(),
            identity: "worker-1".into(),
        })));
        assert!(is_denied(&p.check_operation(&GitOperation::Push {
            branch: "release/2026-10".into(),
            remote: "origin".into(),
            identity: "worker-1".into(),
        })));
    }

    #[test]
    fn merge_into_protected_denied() {
        let p = test_policy();
        assert!(is_denied(&p.check_operation(&GitOperation::Merge {
            source: "wt/rec1".into(),
            target: "master".into(),
            identity: "worker-1".into(),
        })));
        assert!(p
            .check_operation(&GitOperation::Merge {
                source: "wt/a".into(),
                target: "wt/b".into(),
                identity: "worker-1".into(),
            })
            .is_ok());
    }

    #[test]
    fn tags_gated_by_policy() {
        let p = test_policy();
        assert!(is_denied(&p.check_operation(&GitOperation::CreateTag {
            tag: "v1.0".into(),
            identity: "worker-1".into(),
        })));
        let mut open = test_policy();
        open.tags_require_review = false;
        assert!(open
            .check_operation(&GitOperation::CreateTag {
                tag: "v1.0".into(),
                identity: "worker-1".into(),
            })
            .is_ok());
    }

    #[test]
    fn delete_branch_protected_denied() {
        let p = test_policy();
        assert!(is_denied(&p.check_operation(&GitOperation::DeleteBranch {
            branch: "master".into(),
            identity: "worker-1".into(),
        })));
        assert!(p
            .check_operation(&GitOperation::DeleteBranch {
                branch: "wt/old".into(),
                identity: "worker-1".into(),
            })
            .is_ok());
    }

    // --- GitInspector tests against throwaway repos ---

    fn fresh_repo(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rec1-gitpolicy-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&dir)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap()
        };
        assert!(run(&["init", "-q", "-b", "master"]).status.success());
        std::fs::write(dir.join("f.txt"), "x").unwrap();
        assert!(run(&["add", "f.txt"]).status.success());
        assert!(run(&["commit", "-qm", "init"]).status.success());
        dir
    }

    #[test]
    fn inspector_head_and_branch() {
        let dir = fresh_repo("head");
        let head = GitInspector::head_commit(&dir).unwrap();
        assert_eq!(head.len(), 40);
        assert_eq!(GitInspector::current_branch(&dir).unwrap(), "master");
        assert!(GitInspector::branch_exists(&dir, "master").unwrap());
        assert!(!GitInspector::branch_exists(&dir, "nope").unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inspector_rejects_option_inject() {
        let dir = fresh_repo("inject");
        assert!(GitInspector::branch_exists(&dir, "--help").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
