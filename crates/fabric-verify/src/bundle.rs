//! T090 — verifiable ledger bundle export and restore.
//!
//! A bundle is a set of artifacts plus a manifest carrying their digests.
//! [`export_bundle`] validates hashes and sequence continuity at export
//! time; [`restore_bundle`] re-validates everything — including the schema
//! version — and rejects tampered bundles with typed errors.
//!
//! Digests are computed through [`crate::artifacts::ContentDigest`]
//! (injected; the integrator wires SHA-256 — see `artifacts.rs`).

use crate::artifacts::{ContentDigest, StoredArtifact};
use fabric_types::{FabricError, Result};
use serde::{Deserialize, Serialize};

/// Bundle manifest schema version understood by this implementation.
/// Bundles with any other schema version are rejected on restore.
pub const BUNDLE_SCHEMA_VERSION: u32 = 1;

/// One manifest entry: the digest of the artifact at sequence `seq`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleEntry {
    pub seq: u64,
    pub artifact_digest: String,
    pub media_type: String,
    pub size_bytes: u64,
}

/// Manifest: digests plus sequence continuity metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleManifest {
    pub schema_version: u32,
    pub bundle_id: String,
    pub created_at_ms: u64,
    /// Entries in seq order; seqs must be exactly 0..entries.len().
    pub entries: Vec<BundleEntry>,
    /// Digest of the previous bundle in a chain, if chained.
    pub prev_bundle_digest: Option<String>,
}

/// One artifact carried inside a bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleArtifact {
    /// Claimed digest (`algo:hex`); re-validated on export and restore.
    pub digest: String,
    pub media_type: String,
    pub body: Vec<u8>,
}

impl BundleArtifact {
    /// Lift a stored artifact into bundle form.
    pub fn from_stored(a: &StoredArtifact) -> Self {
        Self {
            digest: a.digest.clone(),
            media_type: a.media_type.clone(),
            body: a.body.clone(),
        }
    }
}

/// An exported, validated bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerBundle {
    pub manifest: BundleManifest,
    pub artifacts: Vec<BundleArtifact>,
}

/// A bundle that passed full restore-time validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredBundle {
    pub manifest: BundleManifest,
    pub artifacts: Vec<BundleArtifact>,
}

/// Export a verifiable bundle.
///
/// - Recomputes every artifact's digest and rejects mismatches.
/// - Assigns dense seqs `0..n` in deterministic (digest-sorted) order.
/// - `bundle_id` must be non-empty; `created_at_ms` is caller-injected.
pub fn export_bundle(
    bundle_id: impl Into<String>,
    artifacts: Vec<BundleArtifact>,
    prev_bundle_digest: Option<String>,
    created_at_ms: u64,
    digester: &dyn ContentDigest,
) -> Result<LedgerBundle> {
    let bundle_id = bundle_id.into();
    if bundle_id.is_empty() || bundle_id.len() > 256 {
        return Err(FabricError::InvalidInput {
            field: "bundle_id",
            reason: "length must be 1..=256 characters".to_string(),
        });
    }
    if let Some(prev) = &prev_bundle_digest {
        if prev.is_empty() {
            return Err(FabricError::InvalidInput {
                field: "prev_bundle_digest",
                reason: "must be non-empty when present".to_string(),
            });
        }
    }

    // Validate hashes first: every claimed digest must match the body.
    // A bundle is a set: duplicate digests are a caller error, not deduped
    // silently.
    let mut seen = std::collections::HashSet::new();
    for a in &artifacts {
        if !seen.insert(a.digest.clone()) {
            return Err(FabricError::InvalidInput {
                field: "artifacts",
                reason: "duplicate artifact digest in bundle".to_string(),
            });
        }
        let actual = digester.digest_ref(&a.body);
        if actual != a.digest {
            return Err(FabricError::VerificationFailure(format!(
                "bundle export: digest mismatch for artifact claimed as {}",
                truncate_diag(&a.digest),
            )));
        }
    }

    // Deterministic order, then dense seqs: continuity by construction.
    let mut sorted = artifacts;
    sorted.sort_by(|a, b| a.digest.cmp(&b.digest));
    let entries = sorted
        .iter()
        .enumerate()
        .map(|(i, a)| BundleEntry {
            seq: i as u64,
            artifact_digest: a.digest.clone(),
            media_type: a.media_type.clone(),
            size_bytes: a.body.len() as u64,
        })
        .collect();

    Ok(LedgerBundle {
        manifest: BundleManifest {
            schema_version: BUNDLE_SCHEMA_VERSION,
            bundle_id,
            created_at_ms,
            entries,
            prev_bundle_digest,
        },
        artifacts: sorted,
    })
}

/// Restore a bundle, re-validating everything. Rejects:
/// - unknown schema versions ([`FabricError::InvalidInput`]),
/// - digest mismatches or entry/artifact divergence
///   ([`FabricError::VerificationFailure`]),
/// - sequence gaps or size mismatches ([`FabricError::InvalidInput`]).
pub fn restore_bundle(
    bundle: &LedgerBundle,
    digester: &dyn ContentDigest,
) -> Result<RestoredBundle> {
    let m = &bundle.manifest;
    if m.schema_version != BUNDLE_SCHEMA_VERSION {
        return Err(FabricError::InvalidInput {
            field: "schema_version",
            reason: format!(
                "unsupported bundle schema version {}; this build understands {}",
                m.schema_version, BUNDLE_SCHEMA_VERSION
            ),
        });
    }

    // Sequence continuity: seqs must be exactly 0..n in entry order.
    for (i, e) in m.entries.iter().enumerate() {
        if e.seq != i as u64 {
            return Err(FabricError::InvalidInput {
                field: "entries",
                reason: format!(
                    "sequence gap: entry {} has seq {}, expected {}",
                    i, e.seq, i
                ),
            });
        }
    }

    // Every entry must resolve to exactly one artifact with a matching
    // digest, media type, and size; every artifact must be referenced.
    if bundle.artifacts.len() != m.entries.len() {
        return Err(FabricError::VerificationFailure(format!(
            "bundle manifest lists {} entries but carries {} artifacts",
            m.entries.len(),
            bundle.artifacts.len()
        )));
    }
    for e in &m.entries {
        let a = bundle
            .artifacts
            .iter()
            .find(|a| a.digest == e.artifact_digest)
            .ok_or_else(|| {
                FabricError::VerificationFailure(format!(
                    "bundle entry seq {} references missing artifact {}",
                    e.seq,
                    truncate_diag(&e.artifact_digest),
                ))
            })?;
        let actual = digester.digest_ref(&a.body);
        if actual != a.digest {
            return Err(FabricError::VerificationFailure(format!(
                "bundle artifact {} failed digest re-validation (tampered)",
                truncate_diag(&a.digest),
            )));
        }
        if a.media_type != e.media_type {
            return Err(FabricError::VerificationFailure(format!(
                "bundle entry seq {} media type mismatch",
                e.seq
            )));
        }
        if a.body.len() as u64 != e.size_bytes {
            return Err(FabricError::VerificationFailure(format!(
                "bundle entry seq {} size mismatch: manifest {} vs body {}",
                e.seq,
                e.size_bytes,
                a.body.len()
            )));
        }
    }

    Ok(RestoredBundle {
        manifest: m.clone(),
        artifacts: bundle.artifacts.clone(),
    })
}

/// UTF-8-safe diagnostic truncation.
fn truncate_diag(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.char_indices() {
        if i >= 64 {
            out.push('…');
            break;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::test_support::TestDigest;

    fn digester() -> TestDigest {
        TestDigest
    }

    fn artifact(body: &[u8], media_type: &str) -> BundleArtifact {
        let d = digester();
        BundleArtifact {
            digest: d.digest_ref(body),
            media_type: media_type.to_string(),
            body: body.to_vec(),
        }
    }

    fn sample_bundle() -> LedgerBundle {
        export_bundle(
            "bundle-1",
            vec![
                artifact(b"alpha", "text/plain"),
                artifact(b"beta", "application/json"),
            ],
            None,
            1_700_000_000_000,
            &digester(),
        )
        .unwrap()
    }

    #[test]
    fn export_assigns_dense_seqs_in_digest_order() {
        let b = sample_bundle();
        assert_eq!(b.manifest.schema_version, BUNDLE_SCHEMA_VERSION);
        assert_eq!(b.manifest.bundle_id, "bundle-1");
        let seqs: Vec<u64> = b.manifest.entries.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![0, 1]);
        let digests: Vec<&str> = b.manifest.entries.iter().map(|e| e.artifact_digest.as_str()).collect();
        let mut sorted = digests.clone();
        sorted.sort_unstable();
        assert_eq!(digests, sorted);
    }

    #[test]
    fn restore_accepts_valid_bundle() {
        let b = sample_bundle();
        let r = restore_bundle(&b, &digester()).unwrap();
        assert_eq!(r.manifest, b.manifest);
        assert_eq!(r.artifacts.len(), 2);
    }

    #[test]
    fn export_rejects_duplicate_digests() {
        let a = artifact(b"alpha", "text/plain");
        let err = export_bundle("b", vec![a.clone(), a], None, 1, &digester()).unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }));
    }

    #[test]
    fn export_rejects_digest_mismatch() {
        let mut a = artifact(b"alpha", "text/plain");
        a.digest = "fnv1a64-test:0000000000000000".to_string();
        let err = export_bundle("b", vec![a], None, 1, &digester()).unwrap_err();
        assert!(matches!(err, FabricError::VerificationFailure(_)));
    }

    #[test]
    fn restore_rejects_tampered_body() {
        let mut b = sample_bundle();
        b.artifacts[0].body.push(b'X'); // tamper after export
        let err = restore_bundle(&b, &digester()).unwrap_err();
        assert!(matches!(err, FabricError::VerificationFailure(_)));
        assert!(err.to_string().contains("tampered"));
    }

    #[test]
    fn restore_rejects_unknown_schema() {
        let mut b = sample_bundle();
        b.manifest.schema_version = 999;
        let err = restore_bundle(&b, &digester()).unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }));
        assert!(err.to_string().contains("schema version"));
    }

    #[test]
    fn restore_rejects_sequence_gap() {
        let mut b = sample_bundle();
        b.manifest.entries[1].seq = 7;
        let err = restore_bundle(&b, &digester()).unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }));
    }

    #[test]
    fn restore_rejects_orphan_entry() {
        let mut b = sample_bundle();
        b.artifacts.pop();
        let err = restore_bundle(&b, &digester()).unwrap_err();
        assert!(matches!(err, FabricError::VerificationFailure(_)));
    }

    #[test]
    fn restore_rejects_size_mismatch() {
        let mut b = sample_bundle();
        // Keep digest valid by re-exporting semantics: craft an entry whose
        // size disagrees with the body while the digest still matches.
        let d = digester();
        let body = b"gamma".to_vec();
        let digest = d.digest_ref(&body);
        b.artifacts.push(BundleArtifact {
            digest: digest.clone(),
            media_type: "text/plain".to_string(),
            body,
        });
        b.manifest.entries.push(BundleEntry {
            seq: 2,
            artifact_digest: digest,
            media_type: "text/plain".to_string(),
            size_bytes: 999, // wrong
        });
        let err = restore_bundle(&b, &digester()).unwrap_err();
        assert!(matches!(err, FabricError::VerificationFailure(_)));
    }

    #[test]
    fn export_rejects_empty_bundle_id() {
        let err = export_bundle("", vec![], None, 1, &digester()).unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }));
    }

    #[test]
    fn bundle_serde_round_trip() {
        let b = sample_bundle();
        let json = serde_json::to_string(&b).unwrap();
        let back: LedgerBundle = serde_json::from_str(&json).unwrap();
        assert_eq!(back, b);
        let r = restore_bundle(&back, &digester()).unwrap();
        assert_eq!(r.artifacts.len(), 2);
    }

    #[test]
    fn from_stored_lifts_artifact() {
        use crate::artifacts::{ArtifactStore, StoreConfig};
        let mut s = ArtifactStore::new(StoreConfig {
            max_output_bytes: 1024,
            digester: Box::new(TestDigest),
        })
        .unwrap();
        let stored = s.store_response(b"data", "text/plain", "w").unwrap();
        let ba = BundleArtifact::from_stored(&stored);
        assert_eq!(ba.digest, stored.digest);
        assert_eq!(ba.body, b"data");
    }
}
