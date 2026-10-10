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
}
