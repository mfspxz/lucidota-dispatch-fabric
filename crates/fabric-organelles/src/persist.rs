//! REG-04 — Registry persistence (cell ORG2).
//!
//! Save/load the organelle registry to disk as JSON with digest
//! verification. The file format is:
//!
//! ```json
//! { "records": [ ... ], "registry_digest": "sha256:<hex>" }
//! ```
//!
//! where `registry_digest = "sha256:" + hex(sha256(canonical_json(records)))`
//! and `canonical_json` is `serde_json::to_vec` of the record list (field
//! order = declaration order, deterministic across processes for a fixed
//! struct definition).
//!
//! `load_registry` re-verifies the digest and rejects tampered files with a
//! typed `PersistError::DigestMismatch`. Writes are atomic (temp file +
//! fsync + rename) so a crash mid-save cannot leave a half-written registry.
//!
//! Append-only semantics: the update path is load → register → save. There
//! is no in-place mutation of the file.
//!
//! INTEGRATOR NOTE: reuses the `OrganelleRecord` stand-in from `compose.rs`
//! (see its docs); replace with `crate::record::OrganelleRecord` at
//! integration. Both functions take/return plain record vecs so they work
//! with `registry.all()` directly.

use crate::compose::OrganelleRecord;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use thiserror::Error;

/// Typed persistence errors. A digest mismatch is tamper evidence, not a
/// transport failure — it gets its own variant.
#[derive(Debug, Error)]
pub enum PersistError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("registry digest mismatch: file claims {expected}, recomputed {found}")]
    DigestMismatch { expected: String, found: String },
    #[error("invalid registry file: {0}")]
    InvalidFile(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistryFile {
    records: Vec<OrganelleRecord>,
    registry_digest: String,
}

fn hex(bytes: &[u8]) -> String {
    const CHARS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(CHARS[(b >> 4) as usize] as char);
        s.push(CHARS[(b & 0xf) as usize] as char);
    }
    s
}

/// Compute the registry digest: `"sha256:" + hex(sha256(canonical_json))`.
pub fn registry_digest(records: &[OrganelleRecord]) -> Result<String, PersistError> {
    let canonical = serde_json::to_vec(records)?;
    let mut h = Sha256::new();
    h.update(&canonical);
    Ok(format!("sha256:{}", hex(&h.finalize())))
}

/// Save the registry atomically: temp file + fsync + rename.
pub fn save_registry(records: &[OrganelleRecord], path: &Path) -> Result<(), PersistError> {
    let file = RegistryFile {
        registry_digest: registry_digest(records)?,
        records: records.to_vec(),
    };
    let bytes = serde_json::to_vec_pretty(&file)?;
    let tmp_path = tmp_sibling(path);
    {
        let mut f = File::create(&tmp_path)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Load the registry, verifying the digest. Any tampering — modified records,
/// a forged digest, or a truncated file — is rejected with a typed error.
pub fn load_registry(path: &Path) -> Result<Vec<OrganelleRecord>, PersistError> {
    let bytes = fs::read(path)?;
    let file: RegistryFile = serde_json::from_slice(&bytes).map_err(|e| {
        // Distinguish "not our file at all" from other JSON failures.
        let msg = e.to_string();
        if msg.contains("missing field") {
            PersistError::InvalidFile(format!("not a registry file: {msg}"))
        } else {
            PersistError::Json(e)
        }
    })?;
    if !is_well_formed_digest(&file.registry_digest) {
        return Err(PersistError::InvalidFile(format!(
            "malformed registry_digest: {}",
            file.registry_digest
        )));
    }
    let found = registry_digest(&file.records)?;
    if found != file.registry_digest {
        return Err(PersistError::DigestMismatch {
            expected: file.registry_digest,
            found,
        });
    }
    Ok(file.records)
}

fn tmp_sibling(path: &Path) -> std::path::PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    std::path::PathBuf::from(s)
}

fn is_well_formed_digest(d: &str) -> bool {
    d.len() == 7 + 64
        && d.starts_with("sha256:")
        && d[7..].chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::OrganelleRecord;
    use organelle_core::{Layer, MutationClass, TypeRef};

    fn sample_records() -> Vec<OrganelleRecord> {
        vec![
            OrganelleRecord {
                name: "double".to_string(),
                version: "1.0.0".to_string(),
                layer: Layer::Global,
                mutation_class: MutationClass::ReadOnly,
                inputs: vec![TypeRef::global("Number")],
                outputs: vec![TypeRef::global("Number")],
                interface_digest: "sha256:aa".to_string(),
                artifact_digest: "sha256:bb".to_string(),
                source: "test".to_string(),
            },
            OrganelleRecord {
                name: "render".to_string(),
                version: "2.1.0".to_string(),
                layer: Layer::Domain,
                mutation_class: MutationClass::Write,
                inputs: vec![TypeRef::global("Number")],
                outputs: vec![TypeRef::global("Text")],
                interface_digest: "sha256:cc".to_string(),
                artifact_digest: "sha256:dd".to_string(),
                source: "test".to_string(),
            },
        ]
    }

    fn tmp_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "org2-persist-{}-{}-{}.json",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    #[test]
    fn save_load_round_trip() {
        let records = sample_records();
        let path = tmp_path("roundtrip");
        save_registry(&records, &path).unwrap();
        let loaded = load_registry(&path).unwrap();
        assert_eq!(loaded, records);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn digest_is_stable_across_saves() {
        let records = sample_records();
        let p1 = tmp_path("stable1");
        let p2 = tmp_path("stable2");
        save_registry(&records, &p1).unwrap();
        save_registry(&records, &p2).unwrap();
        let b1 = fs::read(&p1).unwrap();
        let b2 = fs::read(&p2).unwrap();
        assert_eq!(b1, b2, "identical records must serialize identically");
        let _ = fs::remove_file(&p1);
        let _ = fs::remove_file(&p2);
    }

    #[test]
    fn empty_registry_round_trips() {
        let path = tmp_path("empty");
        save_registry(&[], &path).unwrap();
        let loaded = load_registry(&path).unwrap();
        assert!(loaded.is_empty());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn tampered_records_rejected() {
        let records = sample_records();
        let path = tmp_path("tamper");
        save_registry(&records, &path).unwrap();
        // Flip a byte inside the records section (change a version string).
        let mut bytes = fs::read(&path).unwrap();
        let needle = b"\"version\": \"1.0.0\"";
        let pos = bytes
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("fixture must contain the version string");
        bytes[pos + needle.len() - 2] = b'1'; // 1.0.0 -> 1.0.1
        fs::write(&path, &bytes).unwrap();
        let err = load_registry(&path).unwrap_err();
        assert!(
            matches!(err, PersistError::DigestMismatch { .. }),
            "expected DigestMismatch, got {err:?}"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn forged_digest_rejected() {
        let records = sample_records();
        let path = tmp_path("forged");
        save_registry(&records, &path).unwrap();
        let mut text = fs::read_to_string(&path).unwrap();
        let digest = registry_digest(&records).unwrap();
        text = text.replacen(&digest, "sha256:0000000000000000000000000000000000000000000000000000000000000000", 1);
        fs::write(&path, text).unwrap();
        let err = load_registry(&path).unwrap_err();
        assert!(
            matches!(err, PersistError::DigestMismatch { .. }),
            "expected DigestMismatch, got {err:?}"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn garbage_file_rejected() {
        let path = tmp_path("garbage");
        fs::write(&path, b"{ this is not json").unwrap();
        let err = load_registry(&path).unwrap_err();
        assert!(
            matches!(err, PersistError::Json(_) | PersistError::InvalidFile(_)),
            "expected Json/InvalidFile, got {err:?}"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn missing_file_is_io_error() {
        let path = tmp_path("missing");
        let err = load_registry(&path).unwrap_err();
        assert!(
            matches!(err, PersistError::Io(_)),
            "expected Io, got {err:?}"
        );
    }

    #[test]
    fn truncated_file_rejected() {
        let records = sample_records();
        let path = tmp_path("truncated");
        save_registry(&records, &path).unwrap();
        let mut bytes = fs::read(&path).unwrap();
        bytes.truncate(bytes.len() / 2);
        fs::write(&path, &bytes).unwrap();
        assert!(load_registry(&path).is_err());
        let _ = fs::remove_file(&path);
    }
}
