//! T076: detect overlapping changed paths and incompatible public-interface
//! changes before integration.
//!
//! Two checks gate integration:
//! 1. [`find_overlaps`] — a path touched by more than one workspace must be
//!    reconciled, never silently merged.
//! 2. [`api_breaks`] — a workspace whose Rust public-API surface removed or
//!    re-signed an item is an incompatible interface change.
//!
//! Changed-path capture here is a local minimal query (`git diff --name-only`)
//! via [`changed_paths_between`]. W1's T074 diff module owns the canonical
//! normalized-diff capture; the integrator should rewire this module to
//! `diff::ChangedManifest` once it lands. The overlap logic itself is
//! independent of the capture source.

use fabric_types::{FabricError, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

/// Changed paths for one workspace. Paths are repo-relative, sorted, deduped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedPaths {
    pub workspace_id: String,
    pub paths: Vec<String>,
}

impl ChangedPaths {
    pub fn new(workspace_id: impl Into<String>, paths: Vec<String>) -> Self {
        let mut set = BTreeSet::new();
        for p in paths {
            let p = p.trim().trim_start_matches("./").trim_end_matches('/');
            if !p.is_empty() {
                set.insert(p.to_string());
            }
        }
        Self {
            workspace_id: workspace_id.into(),
            paths: set.into_iter().collect(),
        }
    }
}

/// One path touched by more than one workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overlap {
    pub path: String,
    /// Sorted workspace IDs touching the path.
    pub workspace_ids: Vec<String>,
}

/// Find every path touched by two or more workspaces. Deterministic order.
pub fn find_overlaps(workspaces: &[ChangedPaths]) -> Vec<Overlap> {
    let mut by_path: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for w in workspaces {
        for p in &w.paths {
            by_path
                .entry(p.as_str())
                .or_default()
                .insert(w.workspace_id.as_str());
        }
    }
    by_path
        .into_iter()
        .filter(|(_, ws)| ws.len() > 1)
        .map(|(path, ws)| Overlap {
            path: path.to_string(),
            workspace_ids: ws.into_iter().map(str::to_string).collect(),
        })
        .collect()
}

fn run_git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| {
            FabricError::TransportFailure(format!("cannot execute git: {e}"))
        })?;
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
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Changed paths between two commits (local minimal capture; see module docs).
pub fn changed_paths_between(
    repo: &Path,
    workspace_id: &str,
    base: &str,
    head: &str,
) -> Result<ChangedPaths> {
    let out = run_git(repo, &["diff", "--name-only", base, head, "--"])?;
    Ok(ChangedPaths::new(
        workspace_id,
        out.lines().map(str::to_string).collect(),
    ))
}

/// Extract a Rust public-API surface from source text.
///
/// Line-based heuristic, deliberately conservative: only top-level `pub`
/// items (`fn`, `struct`, `enum`, `trait`, `mod`, `const`, `static`, `type`)
/// are captured, as `kind name(signature…)`. Bodies are ignored. This is a
/// tripwire for incompatible interface changes, not a full parser — the
/// heuristic is documented so its limits are explicit.
pub fn rust_api_surface(source: &str) -> Vec<String> {
    let mut items = BTreeSet::new();
    for line in source.lines() {
        let t = line.trim();
        let rest = match t.strip_prefix("pub fn ") {
            Some(r) => Some(("fn", r)),
            None => match t.strip_prefix("pub struct ") {
                Some(r) => Some(("struct", r)),
                None => match t.strip_prefix("pub enum ") {
                    Some(r) => Some(("enum", r)),
                    None => match t.strip_prefix("pub trait ") {
                        Some(r) => Some(("trait", r)),
                        None => match t.strip_prefix("pub mod ") {
                            Some(r) => Some(("mod", r)),
                            None => match t.strip_prefix("pub const ") {
                                Some(r) => Some(("const", r)),
                                None => match t.strip_prefix("pub static ") {
                                    Some(r) => Some(("static", r)),
                                    None => t
                                        .strip_prefix("pub type ")
                                        .map(|r| ("type", r)),
                                },
                            },
                        },
                    },
                },
            },
        };
        if let Some((kind, r)) = rest {
            // name = up to first of `(` `<` `{` `;` `=` `:` ` `; signature = trimmed line cut at `{`
            let name_end = r
                .find(|c: char| ['(', '<', '{', ';', '=', ':', ' '].contains(&c))
                .unwrap_or(r.len());
            let name = r[..name_end].trim();
            if name.is_empty() {
                continue;
            }
            let sig = t.split('{').next().unwrap_or(t).trim().trim_end_matches(';');
            items.insert(format!("{kind} {name} :: {sig}"));
        }
    }
    items.into_iter().collect()
}

/// Kind of incompatible public-interface change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ApiBreakKind {
    /// Item existed in the old surface and is gone in the new one.
    Removed,
    /// Item name survived but its signature text changed.
    SignatureChanged,
}

/// One incompatible public-interface change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiBreak {
    pub item: String,
    pub kind: ApiBreakKind,
}

fn item_key(entry: &str) -> &str {
    // entry = "kind name :: signature"
    entry.split(" :: ").next().unwrap_or(entry)
}

fn item_sig(entry: &str) -> &str {
    entry.split(" :: ").nth(1).unwrap_or("")
}

/// Compare old vs new API surfaces; report removed or re-signed items.
/// Purely additive changes produce no breaks.
pub fn api_breaks(old: &[String], new: &[String]) -> Vec<ApiBreak> {
    let new_map: BTreeMap<&str, &str> =
        new.iter().map(|e| (item_key(e), item_sig(e))).collect();
    let mut breaks = Vec::new();
    for e in old {
        let key = item_key(e);
        match new_map.get(key) {
            None => breaks.push(ApiBreak {
                item: key.to_string(),
                kind: ApiBreakKind::Removed,
            }),
            Some(sig) if *sig != item_sig(e) => breaks.push(ApiBreak {
                item: key.to_string(),
                kind: ApiBreakKind::SignatureChanged,
            }),
            _ => {}
        }
    }
    breaks.sort_by(|a, b| a.item.cmp(&b.item));
    breaks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_overlaps_deterministically() {
        let ws = vec![
            ChangedPaths::new("w1", vec!["a.rs".into(), "b.rs".into()]),
            ChangedPaths::new("w2", vec!["b.rs".into(), "c.rs".into()]),
            ChangedPaths::new("w3", vec!["d.rs".into()]),
        ];
        let ov = find_overlaps(&ws);
        assert_eq!(ov.len(), 1);
        assert_eq!(ov[0].path, "b.rs");
        assert_eq!(ov[0].workspace_ids, vec!["w1", "w2"]);
    }

    #[test]
    fn no_overlap_when_disjoint() {
        let ws = vec![
            ChangedPaths::new("w1", vec!["a.rs".into()]),
            ChangedPaths::new("w2", vec!["b.rs".into()]),
        ];
        assert!(find_overlaps(&ws).is_empty());
    }

    #[test]
    fn changed_paths_normalizes() {
        let c = ChangedPaths::new(
            "w",
            vec!["./b.rs".into(), "a.rs".into(), "b.rs".into(), "".into()],
        );
        assert_eq!(c.paths, vec!["a.rs", "b.rs"]);
    }

    #[test]
    fn api_surface_extracts_pub_items() {
        let src = r#"
pub fn add(a: u32, b: u32) -> u32 { a + b }
fn private() {}
pub struct Config { pub x: u32 }
pub trait Worker { fn run(&self); }
"#;
        let s = rust_api_surface(src);
        assert!(s.iter().any(|i| i.starts_with("fn add ")), "{s:?}");
        assert!(s.iter().any(|i| i.starts_with("struct Config ")), "{s:?}");
        assert!(s.iter().any(|i| i.starts_with("trait Worker ")), "{s:?}");
        assert!(!s.iter().any(|i| i.contains("private")), "{s:?}");
    }

    #[test]
    fn api_breaks_detects_removal_and_resign() {
        let old = rust_api_surface("pub fn f(a: u32) -> u32 { a }\npub fn g() {}\n");
        let new = rust_api_surface("pub fn f(a: u64) -> u64 { a }\n");
        let breaks = api_breaks(&old, &new);
        assert_eq!(breaks.len(), 2);
        assert!(breaks.iter().any(|b| b.item == "fn g"
            && b.kind == ApiBreakKind::Removed));
        assert!(breaks.iter().any(|b| b.item == "fn f"
            && b.kind == ApiBreakKind::SignatureChanged));
    }

    #[test]
    fn additive_change_is_not_a_break() {
        let old = rust_api_surface("pub fn f() {}\n");
        let new = rust_api_surface("pub fn f() {}\npub fn h() {}\n");
        assert!(api_breaks(&old, &new).is_empty());
    }

    #[test]
    fn changed_paths_between_on_throwaway_repo() {
        let d = std::env::temp_dir().join(format!(
            "fabric-ws-overlap-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let g = |a: &[&str]| {
            let o = Command::new("git")
                .arg("-C")
                .arg(&d)
                .args(a)
                .output()
                .unwrap();
            assert!(o.status.success(), "{a:?}: {}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).to_string()
        };
        g(&["init", "-q"]);
        g(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "base"]);
        let base = g(&["rev-parse", "HEAD"]).trim().to_string();
        std::fs::write(d.join("a.rs"), "x").unwrap();
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("sub/b.rs"), "y").unwrap();
        g(&["add", "."]);
        g(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "w"]);
        let head = g(&["rev-parse", "HEAD"]).trim().to_string();
        let c = changed_paths_between(&d, "w1", &base, &head).unwrap();
        assert_eq!(c.paths, vec!["a.rs", "sub/b.rs"]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
