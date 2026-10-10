//! INT-02 — SLSA provenance emission from verdict receipts.
//!
//! Spec implemented: **SLSA Provenance v1**, predicate type
//! `https://slsa.dev/provenance/v1` (spec text at
//! <https://slsa.dev/provenance/v1>, which always resolves to the latest
//! v1.x minor version; page read 2026-10-10 — the v1.1 text). This module
//! emits the predicate BODY only, ready to embed in an in-toto Statement
//! (`_type: https://in-toto.io/Statement/v1`) alongside `subject` and
//! `predicateType`. Signing the envelope (DSSE/cosign) is the caller's job.
//!
//! Mapping from our verdict receipt to the SLSA v1 predicate:
//!
//! - `buildDefinition.buildType` = `{builder.id}/verifier/v1`. Derived from
//!   the caller-supplied builder identity so no identifier is invented; it
//!   names this fabric's verifier-evaluation template for that builder.
//! - `buildDefinition.externalParameters` = `{ requirementId, verdict,
//!   predicates: [{ id, outcome, verifierVersion, command, evidenceDigests }] }`.
//!   External (untrusted) inputs; verified downstream per the spec.
//! - `buildDefinition.internalParameters` = `{ receiptDigest }`.
//!   Platform-controlled; links this provenance to the exact receipt.
//! - `buildDefinition.resolvedDependencies` = the caller-supplied materials,
//!   each as a ResourceDescriptor `{ uri, digest }`.
//! - `runDetails.builder.id` = the caller-supplied builder identity.
//! - `runDetails.metadata.invocationId` = the verdict receipt digest.
//!
//! Unset/empty optional SLSA fields (timestamps, byproducts, builder
//! version) are omitted; per the SLSA parsing rules, unset, null, and empty
//! are equivalent and consumers must ignore unrecognized fields.
//!
//! Worker INT2: bodies implemented against the frozen signatures. The input
//! is `serde_json::Value` (not a struct) so no verdict stand-in type is
//! needed; the expected field names match `verdict.rs`'s `VerdictReceipt`
//! serde shape (`requirement_id`, `predicates`, `overall`,
//! `receipt_digest`; per predicate `predicate_id`, `outcome`, `command`,
//! `verifier_version`, `evidence_digests`).

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SlsaError {
    #[error("verdict incomplete: {0}")]
    IncompleteVerdict(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

/// The SLSA provenance predicate type URI this module emits.
///
/// Implements SLSA Provenance v1 (`https://slsa.dev/provenance/v1`). The URI
/// carries only the major version and always resolves to the latest v1.x
/// minor per the spec's parsing rules.
pub fn provenance_predicate_type() -> &'static str {
    "https://slsa.dev/provenance/v1"
}

/// Builder identity for provenance emitted by this fabric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuilderId {
    pub id: String,
}

/// One material the build consumed: URI + digest map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Material {
    pub uri: String,
    pub digest: std::collections::HashMap<String, String>,
}

/// Verdict outcome strings this module accepts, matching `fabric_types::Verdict`
/// SCREAMING_SNAKE_CASE serialization. Anything else is not a verdict we
/// produced — rejected as incomplete, never coerced.
const KNOWN_OUTCOMES: &[&str] = &[
    "PASS",
    "FAIL",
    "INCONCLUSIVE",
    "BLOCKED",
    "INVALID",
    "INFRA_ERROR",
];

fn get_str<'a>(obj: &'a serde_json::Map<String, serde_json::Value>, field: &str) -> Option<&'a str> {
    obj.get(field).and_then(|v| v.as_str())
}

/// Build an SLSA provenance predicate body (JSON) from a verdict receipt
/// JSON value, a builder identity, and the materials consumed. The verdict
/// value must be a complete verdict receipt (predicate-level outcomes for
/// every predicate); incomplete verdicts are rejected, never defaulted.
pub fn emit_slsa_provenance(
    verdict_receipt: &serde_json::Value,
    builder: &BuilderId,
    materials: &[Material],
) -> Result<serde_json::Value, SlsaError> {
    if builder.id.trim().is_empty() {
        return Err(SlsaError::InvalidInput(
            "builder.id must be non-empty".to_string(),
        ));
    }
    for (i, m) in materials.iter().enumerate() {
        if m.uri.trim().is_empty() {
            return Err(SlsaError::InvalidInput(format!(
                "materials[{i}].uri must be non-empty"
            )));
        }
    }

    let obj = verdict_receipt.as_object().ok_or_else(|| {
        SlsaError::IncompleteVerdict("verdict receipt must be a JSON object".to_string())
    })?;

    let requirement_id = get_str(obj, "requirement_id").ok_or_else(|| {
        SlsaError::IncompleteVerdict("verdict missing non-empty requirement_id".to_string())
    })?;
    if requirement_id.trim().is_empty() {
        return Err(SlsaError::IncompleteVerdict(
            "verdict missing non-empty requirement_id".to_string(),
        ));
    }
    let receipt_digest = get_str(obj, "receipt_digest").ok_or_else(|| {
        SlsaError::IncompleteVerdict("verdict missing non-empty receipt_digest".to_string())
    })?;
    if receipt_digest.trim().is_empty() {
        return Err(SlsaError::IncompleteVerdict(
            "verdict missing non-empty receipt_digest".to_string(),
        ));
    }
    let overall = get_str(obj, "overall").ok_or_else(|| {
        SlsaError::IncompleteVerdict("verdict missing overall outcome".to_string())
    })?;
    if !KNOWN_OUTCOMES.contains(&overall) {
        return Err(SlsaError::IncompleteVerdict(format!(
            "verdict has unknown overall outcome {overall:?}"
        )));
    }
    let predicates = obj
        .get("predicates")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            SlsaError::IncompleteVerdict("verdict missing predicates array".to_string())
        })?;
    if predicates.is_empty() {
        return Err(SlsaError::IncompleteVerdict(
            "verdict predicates array is empty".to_string(),
        ));
    }

    let mut emitted_predicates = Vec::with_capacity(predicates.len());
    for (i, p) in predicates.iter().enumerate() {
        let po = p.as_object().ok_or_else(|| {
            SlsaError::IncompleteVerdict(format!("predicates[{i}] must be an object"))
        })?;
        let pid = get_str(po, "predicate_id").ok_or_else(|| {
            SlsaError::IncompleteVerdict(format!("predicates[{i}] missing predicate_id"))
        })?;
        if pid.trim().is_empty() {
            return Err(SlsaError::IncompleteVerdict(format!(
                "predicates[{i}] has blank predicate_id"
            )));
        }
        let outcome = get_str(po, "outcome").ok_or_else(|| {
            SlsaError::IncompleteVerdict(format!("predicates[{i}] ({pid}) missing outcome"))
        })?;
        if !KNOWN_OUTCOMES.contains(&outcome) {
            return Err(SlsaError::IncompleteVerdict(format!(
                "predicates[{i}] ({pid}) has unknown outcome {outcome:?}"
            )));
        }
        let command = po
            .get("command")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                SlsaError::IncompleteVerdict(format!("predicates[{i}] ({pid}) missing command"))
            })?;
        if command.is_empty() || !command.iter().all(|c| c.as_str().is_some()) {
            return Err(SlsaError::IncompleteVerdict(format!(
                "predicates[{i}] ({pid}) command must be a non-empty string array"
            )));
        }
        let version = get_str(po, "verifier_version").ok_or_else(|| {
            SlsaError::IncompleteVerdict(format!(
                "predicates[{i}] ({pid}) missing verifier_version"
            ))
        })?;
        if version.trim().is_empty() {
            return Err(SlsaError::IncompleteVerdict(format!(
                "predicates[{i}] ({pid}) has blank verifier_version"
            )));
        }
        // Evidence digests must be present (the linkage T087 requires), but
        // may be empty — a FAIL can carry no evidence, and we never invent it.
        let evidence = po
            .get("evidence_digests")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                SlsaError::IncompleteVerdict(format!(
                    "predicates[{i}] ({pid}) missing evidence_digests"
                ))
            })?;
        emitted_predicates.push(serde_json::json!({
            "id": pid,
            "outcome": outcome,
            "verifierVersion": version,
            "command": command,
            "evidenceDigests": evidence,
        }));
    }

    let build_type = format!("{}/verifier/v1", builder.id.trim_end_matches('/'));
    let resolved: Vec<serde_json::Value> = materials
        .iter()
        .map(|m| serde_json::json!({ "uri": m.uri, "digest": m.digest }))
        .collect();

    Ok(serde_json::json!({
        "buildDefinition": {
            "buildType": build_type,
            "externalParameters": {
                "requirementId": requirement_id,
                "verdict": overall,
                "predicates": emitted_predicates,
            },
            "internalParameters": {
                "receiptDigest": receipt_digest,
            },
            "resolvedDependencies": resolved,
        },
        "runDetails": {
            "builder": { "id": builder.id },
            "metadata": { "invocationId": receipt_digest },
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builder() -> BuilderId {
        BuilderId {
            id: "https://builds.example.local/fabric".to_string(),
        }
    }

    fn materials() -> Vec<Material> {
        vec![Material {
            uri: "git+https://example.local/repo@refs/heads/main".to_string(),
            digest: [("gitCommit".to_string(), "abc123".to_string())]
                .into_iter()
                .collect(),
        }]
    }

    fn verdict() -> serde_json::Value {
        serde_json::json!({
            "requirement_id": "REQ-7",
            "overall": "PASS",
            "receipt_digest": "fnv1a64:deadbeef",
            "predicates": [
                {
                    "predicate_id": "p1",
                    "outcome": "PASS",
                    "command": ["cargo", "test"],
                    "verifier_version": "v3",
                    "evidence_digests": ["sha256:aaa"],
                },
                {
                    "predicate_id": "p2",
                    "outcome": "PASS",
                    "command": ["cargo", "clippy"],
                    "verifier_version": "v3",
                    "evidence_digests": [],
                },
            ],
        })
    }

    #[test]
    fn predicate_type_is_slsa_v1_uri() {
        assert_eq!(
            provenance_predicate_type(),
            "https://slsa.dev/provenance/v1"
        );
    }

    #[test]
    fn emitted_predicate_has_slsa_v1_shape() {
        let pred = emit_slsa_provenance(&verdict(), &builder(), &materials()).unwrap();
        let bd = &pred["buildDefinition"];
        assert_eq!(
            bd["buildType"],
            "https://builds.example.local/fabric/verifier/v1"
        );
        assert_eq!(bd["externalParameters"]["requirementId"], "REQ-7");
        assert_eq!(bd["externalParameters"]["verdict"], "PASS");
        assert_eq!(bd["externalParameters"]["predicates"].as_array().unwrap().len(), 2);
        assert_eq!(
            bd["externalParameters"]["predicates"][0]["evidenceDigests"],
            serde_json::json!(["sha256:aaa"])
        );
        assert_eq!(bd["internalParameters"]["receiptDigest"], "fnv1a64:deadbeef");
        let deps = bd["resolvedDependencies"].as_array().unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0]["uri"], "git+https://example.local/repo@refs/heads/main");
        assert_eq!(deps[0]["digest"]["gitCommit"], "abc123");
        let rd = &pred["runDetails"];
        assert_eq!(rd["builder"]["id"], "https://builds.example.local/fabric");
        assert_eq!(rd["metadata"]["invocationId"], "fnv1a64:deadbeef");
    }

    #[test]
    fn materials_round_trip_verbatim() {
        let mats = vec![
            Material {
                uri: "https://example.local/a.tar.gz".to_string(),
                digest: [("sha256".to_string(), "11".to_string())]
                    .into_iter()
                    .collect(),
            },
            Material {
                uri: "https://example.local/b.tar.gz".to_string(),
                digest: [("sha512".to_string(), "22".to_string())]
                    .into_iter()
                    .collect(),
            },
        ];
        let pred = emit_slsa_provenance(&verdict(), &builder(), &mats).unwrap();
        let deps = pred["buildDefinition"]["resolvedDependencies"]
            .as_array()
            .unwrap();
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0]["digest"]["sha256"], "11");
        assert_eq!(deps[1]["digest"]["sha512"], "22");
    }

    #[test]
    fn missing_predicates_rejected() {
        let mut v = verdict();
        v.as_object_mut().unwrap().remove("predicates");
        let err = emit_slsa_provenance(&v, &builder(), &materials()).unwrap_err();
        assert!(matches!(err, SlsaError::IncompleteVerdict(_)));
    }

    #[test]
    fn empty_predicates_rejected() {
        let mut v = verdict();
        v["predicates"] = serde_json::json!([]);
        let err = emit_slsa_provenance(&v, &builder(), &materials()).unwrap_err();
        assert!(matches!(err, SlsaError::IncompleteVerdict(_)));
    }

    #[test]
    fn predicate_missing_outcome_rejected() {
        let mut v = verdict();
        v["predicates"][0].as_object_mut().unwrap().remove("outcome");
        let err = emit_slsa_provenance(&v, &builder(), &materials()).unwrap_err();
        assert!(matches!(err, SlsaError::IncompleteVerdict(_)));
    }

    #[test]
    fn unknown_outcome_rejected_not_coerced() {
        let mut v = verdict();
        v["overall"] = serde_json::json!("MAYBE");
        let err = emit_slsa_provenance(&v, &builder(), &materials()).unwrap_err();
        assert!(matches!(err, SlsaError::IncompleteVerdict(_)));
    }

    #[test]
    fn missing_receipt_digest_rejected() {
        let mut v = verdict();
        v.as_object_mut().unwrap().remove("receipt_digest");
        let err = emit_slsa_provenance(&v, &builder(), &materials()).unwrap_err();
        assert!(matches!(err, SlsaError::IncompleteVerdict(_)));
    }

    #[test]
    fn empty_builder_id_rejected() {
        let err = emit_slsa_provenance(
            &verdict(),
            &BuilderId { id: "  ".to_string() },
            &materials(),
        )
        .unwrap_err();
        assert!(matches!(err, SlsaError::InvalidInput(_)));
    }

    #[test]
    fn empty_material_uri_rejected() {
        let mats = vec![Material {
            uri: "".to_string(),
            digest: Default::default(),
        }];
        let err = emit_slsa_provenance(&verdict(), &builder(), &mats).unwrap_err();
        assert!(matches!(err, SlsaError::InvalidInput(_)));
    }

    #[test]
    fn non_object_verdict_rejected() {
        let err =
            emit_slsa_provenance(&serde_json::json!([1, 2]), &builder(), &materials())
                .unwrap_err();
        assert!(matches!(err, SlsaError::IncompleteVerdict(_)));
    }
}
