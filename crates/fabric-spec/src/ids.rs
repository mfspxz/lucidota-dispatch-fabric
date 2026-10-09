//! T012: deterministic requirement IDs.
//!
//! Every requirement in a [`ParsedSpec`] receives a stable identifier derived
//! purely from content: `REQ-` + the first 16 hex characters of
//! SHA-256(`source_digest | line_start | line_end | text`). The same spec text
//! always yields the same IDs; any change to text or anchors changes the ID,
//! which is what makes source anchors "stable" — drift is detectable.
//!
//! [`assign_requirement_ids`] additionally rejects:
//! - unstable source anchors (via [`detect_unstable_anchors`]), and
//! - ID collisions between distinct requirements ([`FabricError::Conflict`]).

use crate::parser::{ParsedSpec, Requirement};
use fabric_types::{FabricError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

/// T012: validated deterministic requirement identifier (`REQ-[0-9a-f]{16}`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RequirementId(String);

impl RequirementId {
    /// Construct a validated requirement ID.
    pub fn new(s: impl Into<String>) -> Result<Self> {
        let s = s.into();
        if !is_valid_requirement_id(&s) {
            return Err(FabricError::InvalidInput {
                field: "requirement_id",
                reason: format!("must match REQ-[0-9a-f]{{16}}, got {s:?}"),
            });
        }
        Ok(Self(s))
    }

    /// Borrow the identifier text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_valid_requirement_id(s: &str) -> bool {
    let rest = match s.strip_prefix("REQ-") {
        Some(r) => r,
        None => return false,
    };
    rest.len() == 16
        && rest
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl fmt::Display for RequirementId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for RequirementId {
    type Err = FabricError;
    fn from_str(s: &str) -> Result<Self> {
        Self::new(s)
    }
}

impl Serialize for RequirementId {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RequirementId {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

/// One assigned ID together with the requirement index it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignedRequirement {
    pub id: RequirementId,
    pub index: usize,
}

/// Deterministically derive the ID for one requirement.
///
/// Pure function of (`spec.source_digest`, anchor, text): identical inputs
/// always produce the identical ID, on any machine, at any time.
fn requirement_id_for(req: &Requirement, spec_digest: &str) -> RequirementId {
    use sha2::{Digest, Sha256};

    let input = format!(
        "{}|{}|{}|{}",
        spec_digest, req.anchor.line_start, req.anchor.line_end, req.text
    );
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest[..8].iter().map(|b| format!("{:02x}", b)).collect();
    // Invariant: always exactly `REQ-` + 16 lowercase hex chars.
    RequirementId(format!("REQ-{hex}"))
}

/// Indices of requirements whose source anchors are unstable.
///
/// An anchor is unstable when it cannot locate source text: `line_start == 0`
/// (anchors are 1-based), `line_end < line_start` (inverted range), or
/// `line_start` absurdly large (corrupt data, not a real document).
pub fn detect_unstable_anchors(spec: &ParsedSpec) -> Vec<usize> {
    spec.requirements
        .iter()
        .enumerate()
        .filter(|(_, req)| {
            let a = req.anchor;
            a.line_start == 0 || a.line_end < a.line_start || a.line_start > 1_000_000_000
        })
        .map(|(index, _)| index)
        .collect()
}

/// Assign deterministic IDs to every requirement in spec order.
///
/// Fails on unstable anchors ([`FabricError::InvalidInput`] naming the bad
/// indices) and on ID collisions between distinct requirements
/// ([`FabricError::Conflict`]). Note that identical requirement text at
/// different anchors yields different IDs, because the anchor is hashed in —
/// that is intentional, not a collision.
pub fn assign_requirement_ids(spec: &ParsedSpec) -> Result<Vec<AssignedRequirement>> {
    let bad = detect_unstable_anchors(spec);
    if !bad.is_empty() {
        return Err(FabricError::InvalidInput {
            field: "anchors",
            reason: format!("unstable source anchors at requirement indices: {bad:?}"),
        });
    }

    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut assigned = Vec::with_capacity(spec.requirements.len());
    for (index, req) in spec.requirements.iter().enumerate() {
        let id = requirement_id_for(req, &spec.source_digest);
        if let Some(previous) = seen.insert(id.as_str().to_string(), index) {
            return Err(FabricError::Conflict(format!(
                "requirement ID collision: {} assigned to both index {previous} and index {index}",
                id.as_str()
            )));
        }
        assigned.push(AssignedRequirement { id, index });
    }
    Ok(assigned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{parse_spec, SourceAnchor};
    use fabric_types::SpecId;

    fn spec_of(text: &str) -> ParsedSpec {
        parse_spec(text, SpecId::new("spec:test").unwrap()).unwrap()
    }

    #[test]
    fn ids_are_deterministic() {
        let a = assign_requirement_ids(&spec_of("REQ: one\n\nREQ: two\n")).unwrap();
        let b = assign_requirement_ids(&spec_of("REQ: one\n\nREQ: two\n")).unwrap();
        assert_eq!(a, b);
        assert!(a[0].id.as_str().starts_with("REQ-"));
        assert_eq!(a[0].id.as_str().len(), 20);
    }

    #[test]
    fn different_anchors_yield_different_ids() {
        let spec = spec_of("REQ: same text\n\nprose\n\nREQ: same text\n");
        let assigned = assign_requirement_ids(&spec).unwrap();
        assert_ne!(assigned[0].id, assigned[1].id);
    }

    #[test]
    fn text_change_yields_different_id() {
        let a = assign_requirement_ids(&spec_of("REQ: alpha\n")).unwrap();
        let b = assign_requirement_ids(&spec_of("REQ: beta\n")).unwrap();
        assert_ne!(a[0].id, b[0].id);
    }

    #[test]
    fn unstable_anchors_detected_and_rejected() {
        let mut spec = spec_of("REQ: x\n");
        spec.requirements.push(Requirement {
            anchor: SourceAnchor {
                line_start: 0,
                line_end: 0,
            },
            text: "REQ: bad\n".to_string(),
        });
        spec.requirements.push(Requirement {
            anchor: SourceAnchor {
                line_start: 9,
                line_end: 4,
            },
            text: "REQ: inverted\n".to_string(),
        });
        assert_eq!(detect_unstable_anchors(&spec), vec![1, 2]);
        let err = assign_requirement_ids(&spec).unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }));
        assert!(err.to_string().contains("[1, 2]"));
    }

    #[test]
    fn requirement_id_format_validated() {
        assert!(RequirementId::new("REQ-0123456789abcdef").is_ok());
        for bad in [
            "",
            "REQ-",
            "REQ-0123456789abcde",   // too short
            "REQ-0123456789abcdef0",  // too long
            "REQ-0123456789ABCDEF",   // uppercase
            "REQ-0123456789abcdeg",   // non-hex
            "req-0123456789abcdef",   // wrong prefix case
            "T001",
        ] {
            assert!(RequirementId::new(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn deserialization_rejects_invalid_ids() {
        let ok: RequirementId = serde_json::from_str("\"REQ-0123456789abcdef\"").unwrap();
        assert_eq!(ok.as_str(), "REQ-0123456789abcdef");
        let bad: std::result::Result<RequirementId, _> =
            serde_json::from_str("\"not-an-id\"");
        assert!(bad.is_err());
    }

    #[test]
    fn no_collision_on_distinct_requirements() {
        let texts = [
            "REQ: alpha\n",
            "REQ: beta\n",
            "REQ: gamma with a longer body\nsecond line\n",
            "- id: T004\n  objective: delta\n",
        ];
        let joined = texts.join("\n");
        let assigned = assign_requirement_ids(&spec_of(&joined)).unwrap();
        let mut ids: Vec<_> = assigned.iter().map(|a| a.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 4);
    }

    #[test]
    fn display_and_from_str_round_trip() {
        let id = RequirementId::new("REQ-aaaaaaaaaaaaaaaa").unwrap();
        assert_eq!(id.to_string(), "REQ-aaaaaaaaaaaaaaaa");
        let parsed: RequirementId = "REQ-aaaaaaaaaaaaaaaa".parse().unwrap();
        assert_eq!(id, parsed);
    }
}
