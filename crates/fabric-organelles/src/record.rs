//! REG-02 — organelle record and interface digest.
//!
//! An `OrganelleRecord` is the registry's static description of one
//! organelle: identity (name, version), ontological layer, mutation class,
//! typed inputs/outputs (`organelle_core::TypeRef`), the interface digest
//! (what the organelle *is*, independent of declaration order), the artifact
//! digest (what the implementation *was*, set by whoever built it), and the
//! record's source.
//!
//! The interface digest is `sha256:` + hex over a canonical JSON
//! serialization of (name, version, layer, mutation_class, inputs, outputs)
//! with inputs/outputs sorted by their `%layer:Name` string form. Same
//! interface declared in any order → same digest. It does NOT cover
//! `artifact_digest` or `source`: rebuilding the same interface from a
//! different artifact keeps the interface digest stable.

use organelle_core::{Layer, MutationClass, TypeRef};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Stable string form of a mutation class (SCREAMING_SNAKE_CASE, matching
/// fabric-wide enum conventions).
pub fn mutation_class_name(mc: &MutationClass) -> &'static str {
    match mc {
        MutationClass::ReadOnly => "READ_ONLY",
        MutationClass::Write => "WRITE",
        MutationClass::ExternalEffect => "EXTERNAL_EFFECT",
    }
}

/// A registered organelle's static description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganelleRecord {
    /// Unique name, e.g. "fetch-url".
    pub name: String,
    /// Version string, e.g. "1.2.0". A new version is a new registration.
    pub version: String,
    /// Ontological layer (%global / %domain / %client).
    pub layer: Layer,
    /// Allowed side-effect class.
    pub mutation_class: MutationClass,
    /// Declared input types. Order is NOT significant for identity.
    pub inputs: Vec<TypeRef>,
    /// Declared output types. Order is NOT significant for identity.
    pub outputs: Vec<TypeRef>,
    /// `sha256:` + hex over the canonical interface (see
    /// [`compute_interface_digest`]). Computed by [`OrganelleRecord::new`].
    pub interface_digest: String,
    /// Digest of the implementation artifact (opaque to the registry).
    pub artifact_digest: String,
    /// Where this record came from (cell, builder, source file...).
    pub source: String,
}

impl OrganelleRecord {
    /// Build a record, computing the interface digest from the interface.
    /// Takes the full record identity in one call (eight args); the
    /// alternative is a half-initialized builder for a plain data record.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        layer: Layer,
        mutation_class: MutationClass,
        inputs: Vec<TypeRef>,
        outputs: Vec<TypeRef>,
        artifact_digest: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        let mut record = Self {
            name: name.into(),
            version: version.into(),
            layer,
            mutation_class,
            inputs,
            outputs,
            interface_digest: String::new(),
            artifact_digest: artifact_digest.into(),
            source: source.into(),
        };
        record.interface_digest = compute_interface_digest(&record);
        record
    }
}

/// Canonical interface serialization. Field order is fixed by the struct
/// declaration order; `serde_json::to_string` escapes names deterministically,
/// so no name content (newlines, quotes, unicode) can shift field boundaries.
/// Inputs/outputs are sorted by their `%layer:Name` string form.
#[derive(Serialize)]
struct CanonicalInterface<'a> {
    name: &'a str,
    version: &'a str,
    layer: &'a str,
    mutation_class: &'a str,
    inputs: Vec<String>,
    outputs: Vec<String>,
}

/// Compute the interface digest: `sha256:` + lowercase hex(sha256(canonical)).
/// Inputs/outputs are sorted by string form, so declaration order does not
/// affect the digest. Panics only if serialization fails, which cannot happen
/// for this closed shape.
pub fn compute_interface_digest(record: &OrganelleRecord) -> String {
    let mut inputs: Vec<String> = record.inputs.iter().map(|t| t.to_string()).collect();
    inputs.sort();
    let mut outputs: Vec<String> = record.outputs.iter().map(|t| t.to_string()).collect();
    outputs.sort();
    let canonical = CanonicalInterface {
        name: &record.name,
        version: &record.version,
        layer: record.layer.prefix(),
        mutation_class: mutation_class_name(&record.mutation_class),
        inputs,
        outputs,
    };
    let json = serde_json::to_string(&canonical).expect("canonical interface serializes");
    let mut h = Sha256::new();
    h.update(json.as_bytes());
    format!("sha256:{:x}", h.finalize())
}

/// One file's contribution to the artifact digest: relative path plus the
/// sha256 hex of its exact bytes. Field order fixed by declaration order.
#[derive(Serialize)]
struct CanonicalArtifactEntry<'a> {
    path: &'a str,
    sha256: &'a str,
}

/// Recursively collect every `.rs` file under `dir`, skipping symlinks
/// (never followed) and any directory named `target` at any depth.
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("artifact digest: cannot read dir {}: {e}", dir.display()));
    for entry in entries {
        let entry =
            entry.unwrap_or_else(|e| panic!("artifact digest: cannot read entry in {}: {e}", dir.display()));
        let path = entry.path();
        let ft = entry
            .file_type()
            .unwrap_or_else(|e| panic!("artifact digest: cannot stat {}: {e}", path.display()));
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            collect_rs_files(&path, out);
        } else if ft.is_file() && path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Compute the artifact digest of a crate's source tree: `sha256:` +
/// lowercase hex.
///
/// What it covers: every regular `.rs` file under `crate_dir`
/// (recursively), except files under any `target/` directory; symlinks are
/// never followed. Each file contributes `(relative path with forward
/// slashes, sha256 hex of its exact bytes)`; entries are sorted by relative
/// path, so file creation order and traversal order do not affect the
/// digest. Mtimes, absolute paths, permissions, and ownership are never
/// read. An empty tree (or a tree with no `.rs` files) digests the empty
/// entry list — a well-defined value, not an error.
///
/// Why source-tree content and not the compiled rlib: a compiled artifact
/// embeds build-environment details (absolute paths in debuginfo, the exact
/// rustc/codegen version, profile flags) that differ across machines even
/// for identical sources, so "same artifact" is not reproducible by
/// comparing rlibs. The source tree is the reproducible identity: same
/// sources hash the same on any machine, and the digest changes if and only
/// if the sources change. Interface identity (what the organelle *is*)
/// stays in [`compute_interface_digest`]; this is the implementation
/// identity (what the organelle *was built from*). The two digests are
/// separate fields precisely so a rebuild or a source patch moves one
/// without disturbing the other.
///
/// Panics on I/O errors: a digest over a partially-read tree would be
/// silently wrong, so failure is loud rather than approximate.
pub fn compute_artifact_digest(crate_dir: &Path) -> String {
    let mut files = Vec::new();
    collect_rs_files(crate_dir, &mut files);
    let mut entries: Vec<(String, String)> = files
        .iter()
        .map(|p| {
            let rel = p.strip_prefix(crate_dir).unwrap_or(p);
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let bytes = std::fs::read(p).unwrap_or_else(|e| {
                panic!("artifact digest: cannot read {}: {e}", p.display())
            });
            let mut h = Sha256::new();
            h.update(&bytes);
            (rel_str, format!("{:x}", h.finalize()))
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let canonical: Vec<CanonicalArtifactEntry> = entries
        .iter()
        .map(|(p, s)| CanonicalArtifactEntry { path: p, sha256: s })
        .collect();
    let json = serde_json::to_string(&canonical).expect("canonical artifact serializes");
    let mut h = Sha256::new();
    h.update(json.as_bytes());
    format!("sha256:{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> OrganelleRecord {
        OrganelleRecord::new(
            "fetch-url",
            "1.0.0",
            Layer::Domain,
            MutationClass::ExternalEffect,
            vec![TypeRef::domain("Url")],
            vec![TypeRef::global("Text"), TypeRef::domain("Headers")],
            "sha256:artifact",
            "cell-org1",
        )
    }

    #[test]
    fn digest_format_is_sha256_prefixed_hex() {
        let d = compute_interface_digest(&sample());
        assert!(d.starts_with("sha256:"), "digest: {d}");
        assert_eq!(d.len(), 7 + 64, "digest: {d}");
        assert!(d[7..].chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn digest_stable_regardless_of_io_declaration_order() {
        let a = sample();
        let mut b = sample();
        b.inputs.reverse();
        b.outputs.reverse();
        // Recompute after reordering; `new` already computed it once.
        b.interface_digest = compute_interface_digest(&b);
        assert_eq!(a.interface_digest, b.interface_digest);
    }

    #[test]
    fn digest_changes_on_interface_change() {
        let base = compute_interface_digest(&sample());
        let mut changed = sample();
        changed.version = "1.0.1".to_string();
        assert_ne!(base, compute_interface_digest(&changed));
        let mut changed = sample();
        changed.outputs.push(TypeRef::global("Number"));
        assert_ne!(base, compute_interface_digest(&changed));
        let mut changed = sample();
        changed.mutation_class = MutationClass::ReadOnly;
        assert_ne!(base, compute_interface_digest(&changed));
    }

    #[test]
    fn digest_ignores_artifact_and_source() {
        let a = sample();
        let mut b = sample();
        b.artifact_digest = "sha256:different-artifact".to_string();
        b.source = "somewhere-else".to_string();
        assert_eq!(a.interface_digest, compute_interface_digest(&b));
    }

    #[test]
    fn digest_survives_hostile_names() {
        // Names containing field-boundary characters must not collide.
        let mut evil = sample();
        evil.name = "a\"\n,\"layer\":\"%global".to_string();
        let d1 = compute_interface_digest(&evil);
        let mut other = sample();
        other.name = "a".to_string();
        let d2 = compute_interface_digest(&other);
        assert_ne!(d1, d2);
        assert!(d1.starts_with("sha256:"));
    }

    #[test]
    fn new_computes_digest() {
        let r = sample();
        assert_eq!(r.interface_digest, compute_interface_digest(&r));
        assert!(!r.interface_digest.is_empty());
    }

    #[test]
    fn mutation_class_names() {
        assert_eq!(mutation_class_name(&MutationClass::ReadOnly), "READ_ONLY");
        assert_eq!(mutation_class_name(&MutationClass::Write), "WRITE");
        assert_eq!(mutation_class_name(&MutationClass::ExternalEffect), "EXTERNAL_EFFECT");
    }

    #[test]
    fn record_serde_round_trips() {
        let r = sample();
        let json = serde_json::to_string(&r).unwrap();
        let back: OrganelleRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    // --- artifact digest tests -------------------------------------------

    /// Scratch tree outside any git repo. Caller owns cleanup.
    fn scratch_tree(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("reg2-artifact-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_tree(dir: &Path, files: &[(&str, &str)]) {
        for (rel, content) in files {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, content).unwrap();
        }
    }

    #[test]
    fn artifact_digest_known_vector() {
        // Cross-checked against an independent Python implementation of the
        // same canonical form (sorted entries, serde-style compact JSON).
        let dir = scratch_tree("known");
        write_tree(&dir, &[("a.rs", "fn a() {}\n"), ("sub/b.rs", "// b\n")]);
        let d = compute_artifact_digest(&dir);
        assert_eq!(
            d,
            "sha256:750efcbc114d09fdf6840f79d8c8b667dc0d6700cfbe6a26f7ccf6270453d062",
            "digest: {d}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn artifact_digest_empty_tree_is_well_defined() {
        // No .rs files: digests the empty entry list, i.e. sha256("[]").
        let dir = scratch_tree("empty");
        std::fs::write(dir.join("README.md"), b"not rust").unwrap();
        let d = compute_artifact_digest(&dir);
        assert_eq!(
            d,
            "sha256:4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945",
            "digest: {d}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn artifact_digest_stable_and_sensitive() {
        let dir = scratch_tree("stable");
        write_tree(&dir, &[("a.rs", "fn a() {}\n"), ("b.rs", "fn b() {}\n")]);
        let d1 = compute_artifact_digest(&dir);
        let d2 = compute_artifact_digest(&dir);
        assert_eq!(d1, d2, "same tree twice must agree");
        assert!(d1.starts_with("sha256:") && d1.len() == 7 + 64);

        // One byte changes the digest.
        std::fs::write(dir.join("b.rs"), "fn b() {} \n").unwrap();
        let d3 = compute_artifact_digest(&dir);
        assert_ne!(d1, d3, "one byte must move the digest");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn artifact_digest_ignores_creation_order() {
        let d1 = scratch_tree("order1");
        write_tree(&d1, &[("a.rs", "1"), ("sub/b.rs", "2"), ("c.rs", "3")]);
        let d2 = scratch_tree("order2");
        write_tree(&d2, &[("c.rs", "3"), ("a.rs", "1"), ("sub/b.rs", "2")]);
        assert_eq!(
            compute_artifact_digest(&d1),
            compute_artifact_digest(&d2),
            "creation order must not affect the digest"
        );
        let _ = std::fs::remove_dir_all(&d1);
        let _ = std::fs::remove_dir_all(&d2);
    }

    #[test]
    fn artifact_digest_skips_target_dir() {
        let with_target = scratch_tree("withtarget");
        write_tree(
            &with_target,
            &[("src/lib.rs", "lib"), ("target/debug/build/out.rs", "junk")],
        );
        let without_target = scratch_tree("withouttarget");
        write_tree(&without_target, &[("src/lib.rs", "lib")]);
        assert_eq!(
            compute_artifact_digest(&with_target),
            compute_artifact_digest(&without_target),
            "target/ must not contribute"
        );
        // But a non-target nested dir does contribute.
        write_tree(&without_target, &[("nested/x.rs", "lib")]);
        assert_ne!(
            compute_artifact_digest(&with_target),
            compute_artifact_digest(&without_target),
            "real nested sources must contribute"
        );
        let _ = std::fs::remove_dir_all(&with_target);
        let _ = std::fs::remove_dir_all(&without_target);
    }

    #[test]
    fn artifact_digest_ignores_non_rs_files() {
        let dir = scratch_tree("nonrs");
        write_tree(&dir, &[("src/lib.rs", "lib"), ("Cargo.toml", "toml")]);
        let d1 = compute_artifact_digest(&dir);
        std::fs::write(dir.join("notes.txt"), b"noise").unwrap();
        assert_eq!(d1, compute_artifact_digest(&dir), "non-.rs files must not contribute");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
