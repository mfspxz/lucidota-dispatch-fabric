//! T030: versioned immutable registry snapshots.
//!
//! A snapshot is an immutable value: all fields are private and there are no
//! setters, so a captured snapshot cannot be mutated after the fact. The
//! digest covers (version, sorted endpoint set), letting any scheduling
//! decision be reproduced and verified later via [`RegistrySnapshot::verify_digest`].

use crate::endpoints::{Endpoint, EndpointRegistry};
use crate::UnixMillis;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// T030: immutable, versioned view of the endpoint registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistrySnapshot {
    version: u64,
    captured_millis: UnixMillis,
    endpoints: Vec<Endpoint>,
    digest: String,
}

impl RegistrySnapshot {
    /// Capture a snapshot. Endpoints are sorted by ID (not trusting the
    /// registry's order); the digest covers the version and the endpoint set.
    pub fn capture(
        registry: &EndpointRegistry,
        version: u64,
        captured_millis: UnixMillis,
    ) -> Self {
        let mut endpoints: Vec<Endpoint> = registry.all().into_iter().cloned().collect();
        endpoints.sort_by(|a, b| a.id.cmp(&b.id));
        let digest = digest_for(version, &endpoints);
        Self {
            version,
            captured_millis,
            endpoints,
            digest,
        }
    }

    /// Snapshot version, assigned by the caller (monotonic per registry).
    pub fn version(&self) -> u64 {
        self.version
    }

    /// When the snapshot was captured (Unix millis, caller-supplied clock).
    pub fn captured_millis(&self) -> UnixMillis {
        self.captured_millis
    }

    /// The captured endpoints, in deterministic ID order.
    pub fn endpoints(&self) -> &[Endpoint] {
        &self.endpoints
    }

    /// `sha256:` digest over (version, endpoint set).
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Recompute the digest and compare: detects tampering or corruption.
    pub fn verify_digest(&self) -> bool {
        digest_for(self.version, &self.endpoints) == self.digest
    }

    #[cfg(test)]
    pub fn for_test(
        version: u64,
        captured_millis: UnixMillis,
        endpoints: Vec<Endpoint>,
    ) -> Self {
        let digest = digest_for(version, &endpoints);
        Self {
            version,
            captured_millis,
            endpoints,
            digest,
        }
    }
}

/// Canonical digest input: version plus the endpoint set in given order
/// (callers pass ID-sorted slices).
fn digest_for(version: u64, endpoints: &[Endpoint]) -> String {
    #[derive(Serialize)]
    struct Canonical<'a> {
        version: u64,
        endpoints: &'a [Endpoint],
    }
    let canonical =
        serde_json::to_vec(&Canonical { version, endpoints }).expect("snapshot serialization");
    let mut h = Sha256::new();
    h.update(&canonical);
    format!("sha256:{}", hex_of(&h.finalize()))
}

fn hex_of(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoints::{ContextWindow, TransportKind};
    use fabric_types::{EndpointId, ModelId, ProviderId};

    fn endpoint(id: &str) -> Endpoint {
        Endpoint {
            id: EndpointId::new(id).unwrap(),
            provider: ProviderId::new("p").unwrap(),
            model: ModelId::new("m").unwrap(),
            transport: TransportKind::Http,
            capabilities: vec!["coding".to_string()],
            context_window: ContextWindow {
                input_tokens: 8000,
                output_tokens: 2000,
            },
            structured_output: true,
            enabled: true,
        }
    }

    #[test]
    fn digest_is_deterministic() {
        let a = RegistrySnapshot::for_test(7, 1_700_000_000_000, vec![endpoint("ep-a")]);
        let b = RegistrySnapshot::for_test(7, 1_700_000_000_000, vec![endpoint("ep-a")]);
        assert_eq!(a.digest(), b.digest());
        assert!(a.verify_digest());
        assert!(a.digest().starts_with("sha256:"));
    }

    #[test]
    fn digest_covers_version_and_endpoint_set() {
        let base = RegistrySnapshot::for_test(7, 0, vec![endpoint("ep-a")]);
        let other_version = RegistrySnapshot::for_test(8, 0, vec![endpoint("ep-a")]);
        let other_set =
            RegistrySnapshot::for_test(7, 0, vec![endpoint("ep-a"), endpoint("ep-b")]);
        assert_ne!(base.digest(), other_version.digest());
        assert_ne!(base.digest(), other_set.digest());
    }

    #[test]
    fn tampering_is_detected() {
        let s = RegistrySnapshot::for_test(7, 0, vec![endpoint("ep-a")]);
        // Corrupt the serialized form: flip one hex char of the digest.
        let mut v: serde_json::Value = serde_json::to_value(&s).unwrap();
        let d = v["digest"].as_str().unwrap().to_string();
        v["digest"] = serde_json::Value::String(format!(
            "{}{}",
            &d[..d.len() - 1],
            if d.ends_with('0') { '1' } else { '0' }
        ));
        let tampered: RegistrySnapshot = serde_json::from_value(v).unwrap();
        assert!(!tampered.verify_digest());
    }

    #[test]
    fn snapshot_is_immutable_value() {
        let s = RegistrySnapshot::for_test(7, 0, vec![endpoint("ep-a")]);
        let json = serde_json::to_string(&s).unwrap();
        let back: RegistrySnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
        // No setters exist: the only way to "change" a snapshot is to capture a new one.
    }

    #[test]
    #[ignore = "requires R1 cell: EndpointRegistry::register"]
    fn capture_reads_registry_in_id_order() {
        let _ = EndpointRegistry::new();
    }
}
