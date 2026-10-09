//! T061: dispatch envelope.
//!
//! The envelope is the cut-out: it carries everything a worker needs for one
//! attempt (task contract, prompt digest, endpoint, limits, workspace,
//! expected output schema) and nothing else — never the DAG, never sibling
//! tasks, never the project.

use fabric_types::{EndpointId, FabricError, Result, TaskContract};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Per-attempt limits carried in the envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeLimits {
    /// Total-response deadline in milliseconds.
    pub timeout_ms: u64,
    /// Maximum tokens the worker may produce.
    pub max_tokens: u32,
}

/// T061: everything a worker needs for one attempt, and nothing more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchEnvelope {
    pub contract: TaskContract,
    /// sha256 of the exact rendered prompt bytes.
    pub prompt_digest: String,
    pub endpoint: EndpointId,
    pub limits: EnvelopeLimits,
    /// Workspace ID the worker is confined to (see fabric-workspace).
    pub workspace: String,
    /// Expected output schema (JSON Schema) the worker's output is validated
    /// against before acceptance.
    pub output_schema: serde_json::Value,
    /// 1-based attempt number.
    pub attempt: u32,
}

/// Build an envelope. The caller supplies the rendered prompt digest and the
/// output schema; this constructor validates completeness (non-empty prompt
/// digest, non-empty workspace, attempt >= 1).
pub fn create_envelope(
    contract: TaskContract,
    prompt_digest: String,
    endpoint: EndpointId,
    limits: EnvelopeLimits,
    workspace: String,
    output_schema: serde_json::Value,
    attempt: u32,
) -> Result<DispatchEnvelope> {
    if prompt_digest.trim().is_empty() {
        return Err(FabricError::InvalidInput {
            field: "prompt_digest",
            reason: "must be non-empty".to_string(),
        });
    }
    if workspace.trim().is_empty() {
        return Err(FabricError::InvalidInput {
            field: "workspace",
            reason: "must be non-empty".to_string(),
        });
    }
    if attempt == 0 {
        return Err(FabricError::InvalidInput {
            field: "attempt",
            reason: "attempt numbers start at 1".to_string(),
        });
    }
    Ok(DispatchEnvelope {
        contract,
        prompt_digest,
        endpoint,
        limits,
        workspace,
        output_schema,
        attempt,
    })
}

impl DispatchEnvelope {
    /// Envelope identity digest: sha256 over the canonical serialization.
    /// Canonical form is the JSON serialization of the struct (field order is
    /// declaration order, maps are not used), so the digest is stable across
    /// processes and machines. Used for idempotency and receipt linkage.
    pub fn envelope_digest(&self) -> String {
        let bytes =
            serde_json::to_vec(self).expect("DispatchEnvelope serialization cannot fail");
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("sha256:{:x}", hasher.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabric_types::{Scope, TaskClass, TaskId};

    fn contract() -> TaskContract {
        TaskContract::builder(TaskId::new("T061").unwrap(), TaskClass::Implement, "build it")
            .spec_revision("master-spec v2026-10-09")
            .inputs(vec!["in".to_string()])
            .outputs(vec!["out".to_string()])
            .scope(Scope::new(vec!["crates/fabric-dispatch".to_string()]).unwrap())
            .acceptance(vec![fabric_types::AcceptancePredicate::new("p1", "works", true).unwrap()])
            .verifiers(vec!["cargo-test".to_string()])
            .build()
            .unwrap()
    }

    fn envelope() -> DispatchEnvelope {
        create_envelope(
            contract(),
            "sha256:abc123".to_string(),
            EndpointId::new("ep-1").unwrap(),
            EnvelopeLimits {
                timeout_ms: 60_000,
                max_tokens: 4096,
            },
            "ws-42".to_string(),
            serde_json::json!({"type": "object"}),
            1,
        )
        .unwrap()
    }

    #[test]
    fn valid_envelope_builds() {
        let e = envelope();
        assert_eq!(e.attempt, 1);
        assert_eq!(e.workspace, "ws-42");
    }

    #[test]
    fn rejects_incomplete_envelopes() {
        let mk = |digest: &str, workspace: &str, attempt: u32| {
            create_envelope(
                contract(),
                digest.to_string(),
                EndpointId::new("ep-1").unwrap(),
                EnvelopeLimits {
                    timeout_ms: 1000,
                    max_tokens: 100,
                },
                workspace.to_string(),
                serde_json::json!({}),
                attempt,
            )
        };
        assert!(mk("", "ws", 1).is_err());
        assert!(mk("   ", "ws", 1).is_err());
        assert!(mk("sha256:x", "", 1).is_err());
        assert!(mk("sha256:x", "  ", 1).is_err());
        assert!(mk("sha256:x", "ws", 0).is_err());
    }

    #[test]
    fn digest_is_stable_sha256() {
        let e = envelope();
        let d1 = e.envelope_digest();
        let d2 = e.envelope_digest();
        assert_eq!(d1, d2);
        assert!(d1.starts_with("sha256:"));
        assert_eq!(d1.len(), "sha256:".len() + 64);
    }

    #[test]
    fn digest_changes_with_content() {
        let mut e = envelope();
        let d1 = e.envelope_digest();
        e.attempt = 2;
        assert_ne!(d1, e.envelope_digest());
    }
}
