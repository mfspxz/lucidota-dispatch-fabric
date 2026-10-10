//! T080 — per-attempt provenance capture.
//!
//! Record HEAD, dirty state, diff digest, submodule state, and relevant
//! tool versions for each attempt. [`capture_provenance`] is best-effort and
//! READ-ONLY: it never cleans dirty state, never stages, never commits —
//! dirty state is reported, never touched.
//!
//! Content digests use the crate's existing `sha2` dependency (the real
//! content-addressed digest lives in T081's `store.rs`; the integrator
//! should rewire [`diff_digest`] to it once T081 lands).

use fabric_types::{FabricError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;

/// State of one submodule as reported by `git submodule status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmoduleState {
    /// Path of the submodule within the repo.
    pub path: String,
    /// Commit the submodule pointer resolves to.
    pub commit: String,
    /// Whether the submodule is initialized (leading `-` means not).
    pub initialized: bool,
}

/// Tool versions for the environment the attempt runs in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolVersions {
    /// Full `cargo --version` output, None when cargo is unavailable.
    pub cargo: Option<String>,
    /// Full `rustc --version` output, None when rustc is unavailable.
    pub rustc: Option<String>,
    /// Full `git --version` output, None when git is unavailable.
    pub git: Option<String>,
}

/// Provenance captured for one attempt. Non-fatal capture problems are
/// recorded in `capture_errors` rather than failing the capture — an
/// attempt's provenance must never be worse than partial, never missing
/// because a helper command hiccuped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptProvenance {
    /// Attempt number this provenance belongs to.
    pub attempt: u32,
    /// Repository path as passed in.
    pub repo_path: String,
    /// Full HEAD SHA. None when the repo has no commits (unborn HEAD)
    /// or is not a git repository.
    pub head_commit: Option<String>,
    /// Whether the working tree differs from HEAD in any way.
    pub dirty: bool,
    /// Porcelain paths that differ from HEAD, sorted. Empty when clean.
    pub dirty_files: Vec<String>,
    /// `sha256:<hex>` of the exact `git diff HEAD` bytes. The digest of
    /// empty bytes when clean (never omitted, never invented).
    pub diff_digest: String,
    /// Submodule states, in `git submodule status` order.
    pub submodules: Vec<SubmoduleState>,
    /// Tool versions for the attempt environment.
    pub tools: ToolVersions,
    /// Non-fatal problems encountered during capture.
    pub capture_errors: Vec<String>,
}

/// Capture provenance for one attempt. Read-only: every git command issued
/// is a read (`rev-parse`, `status`, `diff`, `submodule status`). Dirty
/// state is reported, never cleaned.
pub fn capture_provenance(repo_path: &Path, attempt: u32) -> AttemptProvenance {
    let mut errors: Vec<String> = Vec::new();

    let head_commit = match git_read(repo_path, &["rev-parse", "HEAD"]) {
        Ok(sha) => Some(sha),
        Err(e) => {
            errors.push(format!("head: {e}"));
            None
        }
    };

    let (dirty, dirty_files) = match git_read(repo_path, &["status", "--porcelain=v1", "-z"]) {
        Ok(out) => {
            // -z: entries are NUL-separated; renames appear as "R  new\0old\0".
            let mut files: Vec<String> = Vec::new();
            let mut parts = out.split('\0').peekable();
            while let Some(entry) = parts.next() {
                if entry.is_empty() {
                    continue;
                }
                // Entry is "XY path" (path may be quoted by git when -z is
                // NOT used; with -z it is raw). Take everything after "XY ".
                if entry.len() > 3 {
                    files.push(entry[3..].to_string());
                }
                // Rename entries carry a second NUL-separated path (the
                // source); record it too so the rename is fully described.
                if entry.starts_with('R') {
                    if let Some(src) = parts.next() {
                        if !src.is_empty() {
                            files.push(src.to_string());
                        }
                    }
                }
            }
            files.sort();
            files.dedup();
            (!files.is_empty(), files)
        }
        Err(e) => {
            errors.push(format!("status: {e}"));
            (false, Vec::new())
        }
    };

    // `git diff HEAD` covers staged + unstaged vs HEAD. When there is no
    // HEAD (no commits), there is nothing meaningful to diff: digest the
    // empty byte string and say so.
    let diff_bytes: Vec<u8> = if head_commit.is_some() {
        match git_read_bytes(repo_path, &["diff", "HEAD"]) {
            Ok(b) => b,
            Err(e) => {
                errors.push(format!("diff: {e}"));
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let diff_digest = format!("sha256:{}", hex_sha256(&diff_bytes));

    let submodules = match git_read(repo_path, &["submodule", "status"]) {
        Ok(out) => parse_submodule_status(&out),
        Err(e) => {
            errors.push(format!("submodule: {e}"));
            Vec::new()
        }
    };

    AttemptProvenance {
        attempt,
        repo_path: repo_path.to_string_lossy().to_string(),
        head_commit,
        dirty,
        dirty_files,
        diff_digest,
        submodules,
        tools: capture_tool_versions(),
        capture_errors: errors,
    }
}

/// sha256 hex of bytes. Local helper; the integrator should rewire callers
/// to T081's `digest_bytes` once the content store lands.
pub fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let out = hasher.finalize();
    let mut s = String::with_capacity(64);
    for b in out {
        s.push(hex_digit(b >> 4));
        s.push(hex_digit(b & 0x0f));
    }
    s
}

fn hex_digit(n: u8) -> char {
    (b"0123456789abcdef"[n as usize]) as char
}

/// Parse `git submodule status` output. Leading character: ' ' initialized
/// and matching, '-' not initialized, '+' checked out at a different
/// commit, 'U' merge conflict. Format per line: `<flag><sha> <path>
/// [(describe)]`.
pub fn parse_submodule_status(output: &str) -> Vec<SubmoduleState> {
    let mut subs = Vec::new();
    for line in output.lines() {
        if line.len() < 42 {
            continue;
        }
        let (flag, rest) = line.split_at(1);
        let mut parts = rest.split_whitespace();
        let commit = match parts.next() {
            Some(c) if c.len() == 40 => c.to_string(),
            _ => continue,
        };
        let path = match parts.next() {
            Some(p) => p.to_string(),
            None => continue,
        };
        subs.push(SubmoduleState {
            path,
            commit,
            initialized: flag != "-",
        });
    }
    subs
}

fn capture_tool_versions() -> ToolVersions {
    ToolVersions {
        cargo: tool_version("cargo"),
        rustc: tool_version("rustc"),
        git: tool_version("git"),
    }
}

fn tool_version(tool: &str) -> Option<String> {
    Command::new(tool)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Run a read-only git command, returning trimmed stdout as a String.
///
/// Only the trailing end is trimmed: leading whitespace is significant
/// (e.g. `git status --porcelain` XY codes start with a space).
fn git_read(repo: &Path, args: &[&str]) -> Result<String> {
    let bytes = git_read_bytes(repo, args)?;
    Ok(String::from_utf8_lossy(&bytes).trim_end().to_string())
}

/// Run a read-only git command, returning raw stdout bytes.
///
/// All call sites pass compile-time literal argument lists, so there is no
/// option-injection surface here (contrast gitpolicy.rs, where branch names
/// are interpolated and validated).
fn git_read_bytes(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| FabricError::TransportFailure(format!("git {} failed to spawn: {e}", args.join(" "))))?;
    if !out.status.success() {
        return Err(FabricError::TransportFailure(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn git_env(cmd: &mut Command) -> &mut Command {
        cmd.env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
    }

    fn fresh_repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rec1-prov-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| git_env(Command::new("git").args(args).current_dir(&dir)).output().unwrap();
        assert!(run(&["init", "-q", "-b", "master"]).status.success());
        std::fs::write(dir.join("f.txt"), "hello").unwrap();
        assert!(run(&["add", "f.txt"]).status.success());
        assert!(run(&["commit", "-qm", "init"]).status.success());
        dir
    }

    #[test]
    fn capture_clean_repo() {
        let dir = fresh_repo("clean");
        let p = capture_provenance(&dir, 1);
        assert_eq!(p.attempt, 1);
        assert!(p.head_commit.as_ref().map(|h| h.len() == 40).unwrap_or(false));
        assert!(!p.dirty);
        assert!(p.dirty_files.is_empty());
        // Digest of the empty diff: sha256 of empty bytes.
        assert_eq!(p.diff_digest, format!("sha256:{}", hex_sha256(b"")));
        assert!(p.submodules.is_empty());
        assert!(p.tools.git.as_ref().map(|g| g.starts_with("git version")).unwrap_or(false));
        assert!(p.capture_errors.is_empty(), "errors: {:?}", p.capture_errors);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn capture_dirty_repo_reports_never_cleans() {
        let dir = fresh_repo("dirty");
        std::fs::write(dir.join("f.txt"), "modified").unwrap();
        std::fs::write(dir.join("new.txt"), "untracked").unwrap();
        let before = std::fs::read_to_string(dir.join("f.txt")).unwrap();
        let p = capture_provenance(&dir, 2);
        assert!(p.dirty);
        assert!(p.dirty_files.contains(&"f.txt".to_string()));
        assert!(p.dirty_files.contains(&"new.txt".to_string()));
        assert_ne!(p.diff_digest, format!("sha256:{}", hex_sha256(b"")));
        // Dirty state reported, never cleaned: the file is untouched.
        assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), before);
        assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "modified");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn capture_non_repo_is_partial_not_fatal() {
        let dir = std::env::temp_dir().join(format!("rec1-prov-notrepo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = capture_provenance(&dir, 3);
        assert!(p.head_commit.is_none());
        assert!(!p.capture_errors.is_empty());
        // Tools are still captured even when the repo is bogus.
        assert!(p.tools.git.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn capture_unborn_head_repo() {
        let dir = std::env::temp_dir().join(format!("rec1-prov-unborn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(Command::new("git")
            .args(["init", "-q", "-b", "master"])
            .current_dir(&dir)
            .output()
            .unwrap()
            .status
            .success());
        let p = capture_provenance(&dir, 4);
        assert!(p.head_commit.is_none());
        assert!(!p.dirty);
        assert_eq!(p.diff_digest, format!("sha256:{}", hex_sha256(b"")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_submodule_status_shapes() {
        let out = " 0123456789abcdef0123456789abcdef01234567 libs/a (v1.0)\n-abcdef0123456789abcdef0123456789abcdef01 libs/b\n+fedcba9876543210fedcba9876543210fedcba98 libs/c\n";
        let subs = parse_submodule_status(out);
        assert_eq!(subs.len(), 3);
        assert_eq!(subs[0].path, "libs/a");
        assert!(subs[0].initialized);
        assert_eq!(subs[1].path, "libs/b");
        assert!(!subs[1].initialized);
        assert!(subs[2].initialized); // '+' means checked out elsewhere, still initialized
        assert!(parse_submodule_status("").is_empty());
        assert!(parse_submodule_status("garbage line\n").is_empty());
    }

    #[test]
    fn hex_sha256_known_vector() {
        // sha256("") known vector.
        assert_eq!(
            hex_sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(hex_sha256(b"abc").len(), 64);
        assert_ne!(hex_sha256(b"a"), hex_sha256(b"b"));
    }

    #[test]
    fn tool_version_missing_tool_is_none() {
        assert!(tool_version("definitely-not-a-real-tool-xyz").is_none());
    }
}
