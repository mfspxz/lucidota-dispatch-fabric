//! T072: compare changed paths with each task's allowed scope.
//!
//! Every path a worker changed is checked against the task contract's
//! `Scope.allowed_paths`. A changed path is authorized only when it equals an
//! allowed path or lies strictly beneath an allowed directory. Anything else
//! — including absolute paths, `..` traversal, and un-normalizable input — is
//! a violation and [`ScopeReport::enforce`] rejects it with `PolicyDenied`.

use fabric_types::{FabricError, Result, Scope, TaskContract};
use serde::{Deserialize, Serialize};

/// Result of checking a task's changed paths against its allowed scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeReport {
    pub task_id: String,
    /// Paths that were checked, as supplied.
    pub checked_paths: Vec<String>,
    /// Changed paths not covered by the allowed scope.
    pub violations: Vec<String>,
}

impl ScopeReport {
    /// True when every changed path was within the allowed scope.
    pub fn is_clean(&self) -> bool {
        self.violations.is_empty()
    }

    /// Reject the attempt when any violation exists.
    pub fn enforce(self) -> Result<()> {
        if self.is_clean() {
            Ok(())
        } else {
            Err(FabricError::PolicyDenied(format!(
                "task {} modified paths outside its allowed scope: {}",
                self.task_id,
                self.violations.join(", ")
            )))
        }
    }
}

/// Normalize a repo-relative path: strip `./`, reject absolute paths,
/// `..` segments, and empty segments. Returns `None` when the path cannot
/// be proven safe.
fn normalize(path: &str) -> Option<String> {
    let path = path.strip_prefix("./").unwrap_or(path);
    let path = path.trim_end_matches('/');
    if path.is_empty() || path.starts_with('/') {
        return None;
    }
    let mut parts = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." | ".." => return None,
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

/// True when normalized `changed` equals `allowed` or sits beneath it.
fn covered_by(allowed: &str, changed: &str) -> bool {
    if changed == allowed {
        return true;
    }
    changed
        .strip_prefix(allowed)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// Check `changed_paths` against the scope in `contract`.
pub fn check_scope(contract: &TaskContract, changed_paths: &[impl AsRef<str>]) -> ScopeReport {
    let allowed: Vec<String> = contract
        .scope
        .allowed_paths
        .iter()
        .filter_map(|a| normalize(a))
        .collect();

    let mut violations = Vec::new();
    let mut checked = Vec::new();
    for raw in changed_paths {
        let raw = raw.as_ref();
        checked.push(raw.to_string());
        match normalize(raw) {
            Some(norm) if allowed.iter().any(|a| covered_by(a, &norm)) => {}
            _ => violations.push(raw.to_string()),
        }
    }

    ScopeReport {
        task_id: contract.id.as_str().to_string(),
        checked_paths: checked,
        violations,
    }
}

/// Convenience: check a bare [`Scope`] without a full contract.
pub fn check_scope_paths(scope: &Scope, changed_paths: &[impl AsRef<str>]) -> Vec<String> {
    let allowed: Vec<String> = scope
        .allowed_paths
        .iter()
        .filter_map(|a| normalize(a))
        .collect();
    changed_paths
        .iter()
        .filter(|raw| match normalize(raw.as_ref()) {
            Some(norm) => !allowed.iter().any(|a| covered_by(a, &norm)),
            None => true,
        })
        .map(|r| r.as_ref().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabric_types::TaskClass;
    use fabric_types::TaskId;

    fn contract_with_scope(paths: &[&str]) -> TaskContract {
        TaskContract::builder(TaskId::new("T072").unwrap(), TaskClass::Implement, "scope test")
            .spec_revision("test")
            .inputs(vec!["in".to_string()])
            .outputs(vec!["out".to_string()])
            .scope(Scope::new(paths.iter().map(|s| s.to_string()).collect()).unwrap())
            .acceptance(vec![fabric_types::AcceptancePredicate::new("p", "d", true).unwrap()])
            .verifiers(vec!["v".to_string()])
            .build()
            .unwrap()
    }

    #[test]
    fn allows_exact_and_nested_paths() {
        let c = contract_with_scope(&["crates/fabric-workspace/src"]);
        let r = check_scope(
            &c,
            &[
                "crates/fabric-workspace/src",
                "crates/fabric-workspace/src/scope.rs",
                "./crates/fabric-workspace/src/deep/nested.rs",
            ],
        );
        assert!(r.is_clean(), "violations: {:?}", r.violations);
        assert!(r.enforce().is_ok());
    }

    #[test]
    fn rejects_sibling_and_parent_escapes() {
        let c = contract_with_scope(&["crates/fabric-workspace/src"]);
        let r = check_scope(
            &c,
            &[
                "crates/fabric-workspace/Cargo.toml", // sibling dir
                "crates/fabric-workspace/src-evil/x.rs", // prefix trick, not a child
                "crates/other/y.rs",
            ],
        );
        assert_eq!(r.violations.len(), 3);
        let err = r.enforce().unwrap_err();
        assert!(
            matches!(err, FabricError::PolicyDenied(_)),
            "expected PolicyDenied, got {err:?}"
        );
    }

    #[test]
    fn rejects_absolute_and_traversal_paths() {
        let c = contract_with_scope(&["crates"]);
        let r = check_scope(
            &c,
            &["/etc/passwd", "../outside.rs", "crates/../../etc/x", ""],
        );
        assert_eq!(r.violations.len(), 4);
    }

    #[test]
    fn multiple_allowed_roots() {
        let c = contract_with_scope(&["crates/a", "tools/b"]);
        let r = check_scope(&c, &["crates/a/x.rs", "tools/b/y.rs", "tools/c/z.rs"]);
        assert_eq!(r.violations, vec!["tools/c/z.rs".to_string()]);
    }

    #[test]
    fn bare_scope_helper_agrees() {
        let scope = Scope::new(vec!["crates/a".to_string()]).unwrap();
        let v = check_scope_paths(&scope, &["crates/a/x.rs", "crates/b/y.rs"]);
        assert_eq!(v, vec!["crates/b/y.rs".to_string()]);
    }
}
