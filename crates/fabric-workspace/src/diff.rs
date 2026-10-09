//! T074: capture normalized diffs and changed-file manifests.
//!
//! Before a workspace is cleaned up, [`CapturedDiff::capture`] snapshots
//! everything that changed relative to the pinned base commit: a normalized
//! unified patch (tracked changes via `git diff`, untracked files as
//! synthesized `/dev/null` patches) and a changed-file manifest. The patch is
//! content-hashed (`sha256:`) and [`CapturedDiff::write_immutable`] persists
//! both files read-only, so post-capture mutation is detectable.

use fabric_types::{ArtifactRef, FabricError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Per-file content cap inside synthesized patches (4 MiB).
const MAX_SYNTH_BYTES: usize = 4 * 1024 * 1024;

/// Status of one changed file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
}

/// One entry in the changed-file manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub status: FileStatus,
    /// Previous path for renames/copies.
    pub old_path: Option<String>,
    /// True when synthesized patch content was truncated or omitted.
    pub content_truncated: bool,
}

/// Manifest portion of a diff capture (serializable, hashable via the patch).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffCapture {
    pub base_commit: String,
    pub head_commit: String,
    pub files: Vec<ChangedFile>,
    /// `sha256:` hex of the normalized patch bytes.
    pub patch_digest: String,
    pub patch_bytes: u64,
}

/// A captured diff: manifest plus the normalized patch text.
#[derive(Debug, Clone)]
pub struct CapturedDiff {
    pub capture: DiffCapture,
    pub patch: String,
}

/// Paths written by [`CapturedDiff::write_immutable`].
#[derive(Debug, Clone)]
pub struct ImmutableArtifacts {
    pub patch_path: PathBuf,
    pub manifest_path: PathBuf,
    pub patch_artifact: ArtifactRef,
}

fn run_git(dir: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| FabricError::TransportFailure(format!("failed to spawn git: {e}")))
}

fn git_stdout(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = run_git(dir, args)?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(FabricError::TransportFailure(format!(
            "git {} failed: {}",
            args.join(" "),
            err.chars().take(300).collect::<String>()
        )));
    }
    Ok(out.stdout)
}

fn status_of(code: &str) -> FileStatus {
    match code.chars().next().unwrap_or('?') {
        'A' => FileStatus::Added,
        'D' => FileStatus::Deleted,
        'R' => FileStatus::Renamed,
        'C' => FileStatus::Copied,
        'T' => FileStatus::TypeChanged,
        _ => FileStatus::Modified,
    }
}

/// Parse `git diff --name-status -z` output into changed files.
fn parse_name_status(raw: &[u8]) -> Vec<ChangedFile> {
    let mut files = Vec::new();
    let mut tokens = raw.split(|b| *b == 0).peekable();
    while let Some(tok) = tokens.next() {
        if tok.is_empty() {
            continue;
        }
        let code = String::from_utf8_lossy(tok);
        let status = status_of(&code);
        if code.starts_with('R') || code.starts_with('C') {
            let old = tokens.next().map(|t| String::from_utf8_lossy(t).into_owned());
            let new = tokens.next().map(|t| String::from_utf8_lossy(t).into_owned());
            if let Some(path) = new {
                files.push(ChangedFile {
                    path,
                    status,
                    old_path: old,
                    content_truncated: false,
                });
            }
        } else if let Some(path) = tokens.next() {
            files.push(ChangedFile {
                path: String::from_utf8_lossy(path).into_owned(),
                status,
                old_path: None,
                content_truncated: false,
            });
        }
    }
    files
}

fn is_binary(sample: &[u8]) -> bool {
    sample.iter().take(8000).any(|b| *b == 0)
}

/// Synthesize a unified patch for an untracked file vs /dev/null.
fn synth_patch_for_untracked(worktree: &Path, rel: &str) -> Result<(String, bool)> {
    let full = worktree.join(rel);
    let bytes = std::fs::read(&full)
        .map_err(|e| FabricError::TransportFailure(format!("failed to read {rel}: {e}")))?;
    let mut out = format!("--- /dev/null\n+++ b/{rel}\n");
    if is_binary(&bytes) {
        out.push_str(&format!("@@ -0,0 +0,0 @@\n# binary file b/{rel}: content omitted\n"));
        return Ok((out, true));
    }
    let text = String::from_utf8_lossy(&bytes);
    let ends_nl = text.ends_with('\n');
    let mut lines: Vec<&str> = text.split('\n').collect();
    if ends_nl {
        lines.pop();
    }
    // Byte-cap the emitted content.
    let mut used = 0usize;
    let mut emitted = Vec::new();
    let mut truncated = false;
    for line in lines {
        if used + line.len() + 1 > MAX_SYNTH_BYTES {
            truncated = true;
            break;
        }
        used += line.len() + 1;
        emitted.push(line);
    }
    out.push_str(&format!("@@ -0,0 +1,{} @@\n", emitted.len()));
    for line in emitted {
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    if !ends_nl && !text.is_empty() {
        out.push_str("\\ No newline at end of file\n");
    }
    if truncated {
        out.push_str("# [truncated: file exceeds 4 MiB]\n");
    }
    Ok((out, truncated || is_binary(&bytes)))
}

impl CapturedDiff {
    /// Capture the normalized diff of `workspace_path` against `base_commit`.
    ///
    /// Normalization: `--no-color --no-ext-diff --unified=3` for tracked
    /// changes; untracked files become synthesized `/dev/null` patches sorted
    /// by path. The returned patch is hashed to form `patch_digest`.
    pub fn capture(workspace_path: &Path, base_commit: &str) -> Result<Self> {
        if base_commit.trim().is_empty() {
            return Err(FabricError::InvalidInput {
                field: "base_commit",
                reason: "must be non-empty".to_string(),
            });
        }
        // Pin the base to a full SHA; the capture is meaningless otherwise.
        let base_raw = git_stdout(
            workspace_path,
            &["rev-parse", "--verify", &format!("{base_commit}^{{commit}}")],
        )?;
        let base_sha = String::from_utf8_lossy(&base_raw).trim().to_string();
        let head_raw = git_stdout(workspace_path, &["rev-parse", "HEAD"])?;
        let head_sha = String::from_utf8_lossy(&head_raw).trim().to_string();

        // Tracked changes (staged + unstaged) vs the base.
        let status_raw = git_stdout(
            workspace_path,
            &["diff", "--name-status", "-z", &base_sha],
        )?;
        let mut files = parse_name_status(&status_raw);

        let mut patch = String::from_utf8_lossy(&git_stdout(
            workspace_path,
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--unified=3",
                &base_sha,
            ],
        )?)
        .into_owned();

        // Untracked files: manifest entries + synthesized patches.
        let others_raw = git_stdout(
            workspace_path,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )?;
        let mut untracked: Vec<String> = others_raw
            .split(|b| *b == 0)
            .filter(|t| !t.is_empty())
            .map(|t| String::from_utf8_lossy(t).into_owned())
            .collect();
        untracked.sort();
        for rel in &untracked {
            let (synth, truncated) = synth_patch_for_untracked(workspace_path, rel)?;
            patch.push_str(&synth);
            files.push(ChangedFile {
                path: rel.clone(),
                status: FileStatus::Added,
                old_path: None,
                content_truncated: truncated,
            });
        }

        files.sort_by(|a, b| a.path.cmp(&b.path));

        let mut hasher = Sha256::new();
        hasher.update(patch.as_bytes());
        let digest = format!("sha256:{:x}", hasher.finalize());

        Ok(Self {
            capture: DiffCapture {
                base_commit: base_sha,
                head_commit: head_sha,
                files,
                patch_digest: digest,
                patch_bytes: patch.len() as u64,
            },
            patch,
        })
    }

    /// Content-addressed reference to the captured patch.
    pub fn patch_artifact(&self, provenance: &str) -> Result<ArtifactRef> {
        ArtifactRef::new(
            self.capture.patch_digest.clone(),
            "text/x-patch",
            self.capture.patch_bytes,
            provenance,
        )
    }

    /// Persist `diff.patch` and `manifest.json` into `dir`, both read-only.
    /// Post-capture mutation is detectable via `patch_digest`.
    pub fn write_immutable(&self, dir: &Path, provenance: &str) -> Result<ImmutableArtifacts> {
        std::fs::create_dir_all(dir)
            .map_err(|e| FabricError::TransportFailure(format!("failed to create {dir:?}: {e}")))?;
        let patch_path = dir.join("diff.patch");
        let manifest_path = dir.join("manifest.json");
        let manifest_json = serde_json::to_string_pretty(&self.capture).map_err(|e| {
            FabricError::MalformedOutput(format!("manifest serialization failed: {e}"))
        })?;
        for (path, bytes) in [(&patch_path, self.patch.as_bytes()), (&manifest_path, manifest_json.as_bytes())] {
            std::fs::write(path, bytes).map_err(|e| {
                FabricError::TransportFailure(format!("failed to write {}: {e}", path.display()))
            })?;
            let mut perms = std::fs::metadata(path)
                .map_err(|e| FabricError::TransportFailure(format!("stat failed: {e}")))?
                .permissions();
            perms.set_readonly(true);
            std::fs::set_permissions(path, perms).map_err(|e| {
                FabricError::TransportFailure(format!("chmod failed: {e}"))
            })?;
        }
        Ok(ImmutableArtifacts {
            patch_path,
            manifest_path,
            patch_artifact: self.patch_artifact(provenance)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(1000);

    fn temp_repo() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "w1-diff-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git").arg("-C").arg(&dir).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "w1@test"]);
        git(&["config", "user.name", "w1"]);
        git(&["config", "commit.gpgsign", "false"]);
        fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        fs::write(dir.join("b.txt"), "bye\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "seed"]);
        dir
    }

    fn head(repo: &Path) -> String {
        let out = Command::new("git").arg("-C").arg(repo).args(["rev-parse", "HEAD"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn captures_modified_added_and_deleted() {
        let repo = temp_repo();
        let base = head(&repo);
        fs::write(repo.join("a.txt"), "one\nTWO\n").unwrap(); // modified
        fs::remove_file(repo.join("b.txt")).unwrap(); // deleted
        fs::write(repo.join("new.txt"), "hello\n").unwrap(); // untracked
        fs::create_dir_all(repo.join("sub")).unwrap();
        fs::write(repo.join("sub/deep.txt"), "deep\n").unwrap(); // untracked nested

        let cap = CapturedDiff::capture(&repo, &base).unwrap();
        assert_eq!(cap.capture.base_commit, base);
        assert_eq!(cap.capture.head_commit, base); // nothing committed

        let paths: Vec<(&str, FileStatus)> = cap
            .capture
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.status))
            .collect();
        assert!(paths.contains(&("a.txt", FileStatus::Modified)), "{paths:?}");
        assert!(paths.contains(&("b.txt", FileStatus::Deleted)), "{paths:?}");
        assert!(paths.contains(&("new.txt", FileStatus::Added)), "{paths:?}");
        assert!(paths.contains(&("sub/deep.txt", FileStatus::Added)), "{paths:?}");

        // Patch contains the tracked diff and synthesized untracked hunks.
        assert!(cap.patch.contains("TWO"));
        assert!(cap.patch.contains("+++ b/new.txt"));
        assert!(cap.patch.contains("+hello"));

        // Deterministic: second capture hashes identically.
        let cap2 = CapturedDiff::capture(&repo, &base).unwrap();
        assert_eq!(cap.capture.patch_digest, cap2.capture.patch_digest);
        assert!(cap.capture.patch_digest.starts_with("sha256:"));

        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn empty_diff_is_stable() {
        let repo = temp_repo();
        let base = head(&repo);
        let cap = CapturedDiff::capture(&repo, &base).unwrap();
        assert!(cap.capture.files.is_empty());
        assert_eq!(cap.capture.patch_bytes, 0);
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    #[allow(clippy::permissions_set_readonly_false)] // test cleanup must restore writability
    fn write_immutable_is_readonly_and_addressed() {
        let repo = temp_repo();
        let base = head(&repo);
        fs::write(repo.join("a.txt"), "one\nTWO\n").unwrap();
        let cap = CapturedDiff::capture(&repo, &base).unwrap();

        let out = repo.join("artifacts");
        let arts = cap.write_immutable(&out, "w1-test").unwrap();
        assert!(arts.patch_path.is_file());
        assert!(arts.manifest_path.is_file());
        assert!(arts.patch_path.metadata().unwrap().permissions().readonly());
        assert!(arts.manifest_path.metadata().unwrap().permissions().readonly());

        // Manifest round-trips and digest matches the patch bytes.
        let manifest: DiffCapture =
            serde_json::from_str(&fs::read_to_string(&arts.manifest_path).unwrap()).unwrap();
        assert_eq!(manifest, cap.capture);
        let patch_bytes = fs::read(&arts.patch_path).unwrap();
        let mut h = Sha256::new();
        h.update(&patch_bytes);
        assert_eq!(manifest.patch_digest, format!("sha256:{:x}", h.finalize()));
        assert_eq!(arts.patch_artifact.digest, manifest.patch_digest);

        // Cleanup: restore writability before removing the temp dir.
        for p in [&arts.patch_path, &arts.manifest_path] {
            let mut perms = p.metadata().unwrap().permissions();
            perms.set_readonly(false);
            fs::set_permissions(p, perms).unwrap();
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn rejects_bad_base() {
        let repo = temp_repo();
        assert!(CapturedDiff::capture(&repo, "deadbeef").is_err());
        let _ = fs::remove_dir_all(&repo);
    }
}
