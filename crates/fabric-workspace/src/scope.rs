//! T072: compare changed paths with each task's allowed scope.
//!
//! Every path a worker changed is checked against the task contract's
//! `Scope.allowed_paths`. A changed path is authorized only when it equals an
//! allowed path or lies strictly beneath an allowed directory. Anything else
//! — including absolute paths, `..` traversal, and un-normalizable input — is
//! a violation and [`ScopeReport::enforce`] rejects it with `PolicyDenied`.

use fabric_types::{FabricError, Result, Scope, TaskContract};
use serde::{Deserialize, Serialize};
use std::path::Path;

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

/// List paths changed inside the workspace, relative to the workspace root.
///
/// Runs `git -C <workspace> status --porcelain=v1`; untracked, modified,
/// and staged changes all count. Fail-closed: any git failure, non-UTF-8
/// output, or unparseable entry is an `Err` — never a silently empty list.
/// (An empty `Ok` means a genuinely clean tree.)
pub fn workspace_changed_paths(workspace: &Path) -> Result<Vec<String>> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .output()
        .map_err(|e| {
            FabricError::VerificationFailure(format!(
                "scope diff: git status failed to spawn for {}: {e}",
                workspace.display()
            ))
        })?;
    if !out.status.success() {
        return Err(FabricError::VerificationFailure(format!(
            "scope diff: git status failed for {}: {}",
            workspace.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let text = String::from_utf8(out.stdout).map_err(|e| {
        FabricError::VerificationFailure(format!("scope diff: git status output not UTF-8: {e}"))
    })?;
    parse_porcelain(&text)
}

/// Parse `git status --porcelain=v1` output into workspace-relative paths.
/// Rename/copy entries (`R  old -> new`, quoted or bare) yield the
/// destination path.
fn parse_porcelain(text: &str) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    for line in text.lines() {
        if line.len() < 4 || line.as_bytes()[2] != b' ' {
            return Err(FabricError::VerificationFailure(format!(
                "scope diff: unparseable porcelain line {line:?}"
            )));
        }
        paths.push(parse_path_spec(&line[3..])?);
    }
    Ok(paths)
}

/// Parse one porcelain path spec: bare, C-quoted, or a rename pair
/// (`old -> new`, each side bare or quoted). Returns the effective path —
/// the rename destination when present.
fn parse_path_spec(spec: &str) -> Result<String> {
    let spec = spec.trim();
    // A rename pair is `old -> new` with each side independently bare or
    // C-quoted (git quotes each path on its own merits). Find the last
    // ` -> ` outside quotes; the right side is the rename destination.
    // (A bare filename cannot contain an unquoted ` -> `.)
    let bytes = spec.as_bytes();
    let mut in_quotes = false;
    let mut escaped = false;
    let mut split_at: Option<usize> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if escaped {
            escaped = false;
        } else if b == b'\\' && in_quotes {
            escaped = true;
        } else if b == b'"' {
            in_quotes = !in_quotes;
        } else if !in_quotes && spec[i..].starts_with(" -> ") {
            split_at = Some(i);
        }
        i += 1;
    }
    let target = match split_at {
        Some(at) => spec[at + 4..].trim(),
        None => spec,
    };
    if let Some(inner) = target.strip_prefix('"') {
        let (quoted, _) = split_quoted(inner)?;
        unquote_c(&quoted)
    } else {
        Ok(target.to_string())
    }
}

/// Split `"...` at the closing unescaped quote. Returns (inner, rest).
fn split_quoted(s: &str) -> Result<(String, String)> {
    let bytes = s.as_bytes();
    let mut inner = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => {
                inner.push(bytes[i]);
                inner.push(bytes[i + 1]);
                i += 2;
            }
            b'"' => {
                let rest = s[i + 1..].to_string();
                let inner = String::from_utf8(inner).map_err(|_| {
                    FabricError::VerificationFailure(
                        "scope diff: quoted path not UTF-8".to_string(),
                    )
                })?;
                return Ok((inner, rest));
            }
            b => {
                inner.push(b);
                i += 1;
            }
        }
    }
    Err(FabricError::VerificationFailure(
        "scope diff: unterminated quoted path".to_string(),
    ))
}

/// Undo git's C-style quoting: `\\`, `\"`, `\n`, `\t`, `\r`, and octal
/// `\ooo` byte escapes. Fail-closed on invalid UTF-8 or bad escapes.
fn unquote_c(s: &str) -> Result<String> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        if i >= bytes.len() {
            return Err(FabricError::VerificationFailure(
                "scope diff: trailing backslash in quoted path".to_string(),
            ));
        }
        match bytes[i] {
            b'\\' => out.push(b'\\'),
            b'"' => out.push(b'"'),
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'0'..=b'7' => {
                if i + 2 >= bytes.len() {
                    return Err(FabricError::VerificationFailure(
                        "scope diff: truncated octal escape".to_string(),
                    ));
                }
                let mut val: u32 = 0;
                for k in 0..3 {
                    let d = bytes[i + k];
                    if !(b'0'..=b'7').contains(&d) {
                        return Err(FabricError::VerificationFailure(
                            "scope diff: bad octal escape".to_string(),
                        ));
                    }
                    val = val * 8 + (d - b'0') as u32;
                }
                if val > 255 {
                    return Err(FabricError::VerificationFailure(
                        "scope diff: octal escape out of range".to_string(),
                    ));
                }
                out.push(val as u8);
                i += 2;
            }
            other => {
                return Err(FabricError::VerificationFailure(format!(
                    "scope diff: unknown escape \\{}",
                    other as char
                )));
            }
        }
        i += 1;
    }
    String::from_utf8(out).map_err(|_| {
        FabricError::VerificationFailure("scope diff: quoted path not UTF-8".to_string())
    })
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

    // --- workspace_changed_paths (FIX-1) ---

    use std::sync::atomic::{AtomicU64, Ordering};

    static SCOPE_TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A fresh git repo in the system temp dir (outside any other repo).
    fn temp_git_repo(tag: &str) -> std::path::PathBuf {
        let n = SCOPE_TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "fix1-scope-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "test@fabric"]);
        git(&["config", "user.name", "fabric-test"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("base.txt"), b"base\n").unwrap();
        git(&["add", "base.txt"]);
        git(&["commit", "-q", "-m", "base"]);
        dir
    }

    #[test]
    fn workspace_diff_lists_untracked_modified_and_staged() {
        let dir = temp_git_repo("all");
        // Untracked.
        std::fs::write(dir.join("new.txt"), b"new\n").unwrap();
        // Modified (unstaged).
        std::fs::write(dir.join("base.txt"), b"changed\n").unwrap();
        // Staged new file.
        std::fs::write(dir.join("staged.txt"), b"staged\n").unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["add", "staged.txt"])
            .output()
            .unwrap();

        let mut paths = workspace_changed_paths(&dir).unwrap();
        paths.sort();
        assert_eq!(paths, vec!["base.txt", "new.txt", "staged.txt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn workspace_diff_empty_when_clean() {
        let dir = temp_git_repo("clean");
        assert_eq!(workspace_changed_paths(&dir).unwrap(), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn workspace_diff_git_failure_is_error() {
        // Not a git repo: fail-closed, never a silently empty list.
        let dir = std::env::temp_dir().join(format!(
            "fix1-scope-notrepo-{}-{}",
            std::process::id(),
            SCOPE_TEST_COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let err = workspace_changed_paths(&dir).unwrap_err();
        assert!(
            matches!(err, FabricError::VerificationFailure(_)),
            "expected VerificationFailure, got {err:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn workspace_diff_parses_renames_and_quoted_paths() {
        let dir = temp_git_repo("quoting");
        // Rename a tracked file (staged rename).
        std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["mv", "base.txt", "renamed.txt"])
            .output()
            .unwrap();
        // Untracked file with a space (git quotes it in porcelain).
        std::fs::write(dir.join("with space.txt"), b"x\n").unwrap();

        let mut paths = workspace_changed_paths(&dir).unwrap();
        paths.sort();
        assert_eq!(paths, vec!["renamed.txt", "with space.txt"], "got {paths:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn porcelain_parser_unit_cases() {
        // Bare entries.
        assert_eq!(parse_porcelain("M  a.txt\n?? b.txt\n").unwrap(), vec!["a.txt", "b.txt"]);
        // Bare rename yields the destination.
        assert_eq!(
            parse_porcelain("R  old.txt -> new.txt\n").unwrap(),
            vec!["new.txt"]
        );
        // Quoted rename yields the unquoted destination.
        assert_eq!(
            parse_porcelain("R  \"old name.txt\" -> \"new name.txt\"\n").unwrap(),
            vec!["new name.txt"]
        );
        // Mixed quoting (git quotes each side independently).
        assert_eq!(
            parse_porcelain("R  old.txt -> \"new name.txt\"\n").unwrap(),
            vec!["new name.txt"]
        );
        assert_eq!(
            parse_porcelain("R  \"old name.txt\" -> new.txt\n").unwrap(),
            vec!["new.txt"]
        );
        // Octal escape (git quotes non-ASCII bytes as \ooo).
        assert_eq!(
            parse_porcelain("?? \"caf\\303\\251.txt\"\n").unwrap(),
            vec!["caf\u{e9}.txt"]
        );
        // Malformed line is fail-closed.
        assert!(parse_porcelain("XY\n").is_err());
        assert!(parse_porcelain("?? \"unterminated\n").is_err());
    }
}
