//! T013: typed acceptance predicates.
//!
//! Acceptance conditions embedded in requirement text are extracted into
//! [`TypedPredicate`]s, each optionally carrying an [`EvidencePlan`].
//! [`flag_missing_evidence`] returns every predicate with no testable evidence
//! plan — those requirements cannot be proven and must be flagged, never
//! silently passed.
//!
//! Extraction grammar (deterministic, documented here): each line of the
//! requirement text is examined after trimming. A line is an acceptance
//! condition iff it starts with one of `ACCEPT:`, `AC:`, `MUST `, `SHALL `,
//! `VERIFY:`, `- [ ]` (longer markers win over shorter prefixes, so `ACCEPT:`
//! is tested before `AC:`). The predicate description is the line with the
//! marker stripped and trimmed. If the remainder of the line contains
//! `EVIDENCE:`, `TEST:`, or `PROOF:` (earliest wins), the text after that
//! keyword becomes the evidence plan detail, with kind mapped as
//! `TEST:` → [`EvidenceKind::Test`], `PROOF:` → [`EvidenceKind::Analysis`],
//! `EVIDENCE:` → [`EvidenceKind::Inspection`]. An empty detail yields no plan.

use crate::parser::Requirement;
use fabric_types::{FabricError, Result};
use serde::{Deserialize, Serialize};
use std::fmt;

/// How the predicate's satisfaction will be evidenced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EvidenceKind {
    Test,
    Inspection,
    Demonstration,
    Analysis,
}

impl fmt::Display for EvidenceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Test => "TEST",
            Self::Inspection => "INSPECTION",
            Self::Demonstration => "DEMONSTRATION",
            Self::Analysis => "ANALYSIS",
        };
        f.write_str(name)
    }
}

/// A testable plan for evidencing one predicate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidencePlan {
    pub kind: EvidenceKind,
    pub detail: String,
}

impl EvidencePlan {
    /// Construct a plan; empty detail is rejected (a plan must say how).
    pub fn new(kind: EvidenceKind, detail: impl Into<String>) -> Result<Self> {
        let detail = detail.into();
        if detail.trim().is_empty() {
            return Err(FabricError::InvalidInput {
                field: "evidence_plan",
                reason: "detail must be non-empty".to_string(),
            });
        }
        Ok(Self { kind, detail })
    }
}

/// T013: one typed acceptance condition extracted from a requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedPredicate {
    /// Deterministic: `P-<requirement-index>-<n>`.
    pub id: String,
    pub description: String,
    /// `None` means no testable evidence plan was found — flag it.
    pub evidence_plan: Option<EvidencePlan>,
}

impl TypedPredicate {
    pub fn new(
        id: impl Into<String>,
        description: impl Into<String>,
        evidence_plan: Option<EvidencePlan>,
    ) -> Result<Self> {
        let id = id.into();
        let description = description.into();
        if id.trim().is_empty() {
            return Err(FabricError::InvalidInput {
                field: "predicate.id",
                reason: "must be non-empty".to_string(),
            });
        }
        if description.trim().is_empty() {
            return Err(FabricError::InvalidInput {
                field: "predicate.description",
                reason: "must be non-empty".to_string(),
            });
        }
        Ok(Self {
            id,
            description,
            evidence_plan,
        })
    }
}

/// Acceptance-condition markers, longest first so prefixes don't shadow.
const ACCEPTANCE_MARKERS: &[&str] = &["ACCEPT:", "VERIFY:", "- [ ]", "MUST ", "SHALL ", "AC:"];

/// Evidence keywords mapped to their evidence kind.
const EVIDENCE_KEYWORDS: &[(&str, EvidenceKind)] = &[
    ("EVIDENCE:", EvidenceKind::Inspection),
    ("TEST:", EvidenceKind::Test),
    ("PROOF:", EvidenceKind::Analysis),
];

/// Extract typed predicates from one requirement's text.
///
/// Predicate IDs are deterministic: `P-<index>-<n>` with `n` the 1-based
/// encounter order within the requirement.
pub fn predicates_for_requirement(index: usize, req: &Requirement) -> Vec<TypedPredicate> {
    let mut predicates = Vec::new();
    let mut n = 0u32;
    for line in req.text.lines() {
        let trimmed = line.trim();
        let marker = ACCEPTANCE_MARKERS
            .iter()
            .find(|m| trimmed.starts_with(*m));
        let marker = match marker {
            Some(m) => m,
            None => continue,
        };
        n += 1;
        let rest = trimmed[marker.len()..].trim();
        let (description, evidence_plan) = split_evidence(rest);
        if description.is_empty() {
            // A bare marker with no condition text is not an acceptance
            // condition; skip it rather than minting an empty predicate.
            n -= 1;
            continue;
        }
        predicates.push(TypedPredicate {
            id: format!("P-{index}-{n}"),
            description: description.to_string(),
            evidence_plan,
        });
    }
    predicates
}

/// Split a condition line into its description and optional evidence plan.
fn split_evidence(rest: &str) -> (&str, Option<EvidencePlan>) {
    let mut best: Option<(usize, &str, EvidenceKind)> = None;
    for (keyword, kind) in EVIDENCE_KEYWORDS {
        if let Some(pos) = rest.find(keyword) {
            let earlier = best.is_none_or(|(best_pos, _, _)| pos < best_pos);
            if earlier {
                best = Some((pos, keyword, *kind));
            }
        }
    }
    match best {
        Some((pos, keyword, kind)) => {
            let description = rest[..pos].trim_end();
            let detail = rest[pos + keyword.len()..].trim();
            let plan = (!detail.is_empty()).then(|| EvidencePlan {
                kind,
                detail: detail.to_string(),
            });
            (description, plan)
        }
        None => (rest, None),
    }
}

/// Flag every predicate that lacks a testable evidence plan.
///
/// These are the requirements that cannot be proven; callers must surface
/// them rather than treat them as passing.
pub fn flag_missing_evidence(predicates: &[TypedPredicate]) -> Vec<&TypedPredicate> {
    predicates
        .iter()
        .filter(|p| p.evidence_plan.is_none())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::SourceAnchor;

    fn req_of(text: &str) -> Requirement {
        Requirement {
            anchor: SourceAnchor {
                line_start: 1,
                line_end: 1,
            },
            text: text.to_string(),
        }
    }

    #[test]
    fn extracts_each_marker_kind() {
        let text = "REQ: thing\nACCEPT: boots in under 5s\nAC: responds to ping\nMUST not leak memory\nSHALL log errors\nVERIFY: checksum matches\n- [ ] docs updated\n";
        let preds = predicates_for_requirement(3, &req_of(text));
        assert_eq!(preds.len(), 6);
        assert_eq!(preds[0].id, "P-3-1");
        assert_eq!(preds[0].description, "boots in under 5s");
        assert_eq!(preds[5].id, "P-3-6");
        assert_eq!(preds[5].description, "docs updated");
    }

    #[test]
    fn accept_prefix_does_not_shadow_accept_marker() {
        let preds = predicates_for_requirement(0, &req_of("ACCEPT: full word wins\n"));
        assert_eq!(preds.len(), 1);
        assert_eq!(preds[0].description, "full word wins");
    }

    #[test]
    fn evidence_keywords_map_to_kinds() {
        let text = "REQ: x\nACCEPT: fast TEST: run benches\nACCEPT: safe PROOF: model-check the protocol\nACCEPT: clean EVIDENCE: attach the log\nACCEPT: vague\n";
        let preds = predicates_for_requirement(0, &req_of(text));
        assert_eq!(preds.len(), 4);
        assert_eq!(
            preds[0].evidence_plan,
            Some(EvidencePlan {
                kind: EvidenceKind::Test,
                detail: "run benches".to_string()
            })
        );
        assert_eq!(preds[1].evidence_plan.as_ref().unwrap().kind, EvidenceKind::Analysis);
        assert_eq!(
            preds[2].evidence_plan.as_ref().unwrap().kind,
            EvidenceKind::Inspection
        );
        assert!(preds[3].evidence_plan.is_none());
        // description excludes the evidence clause
        assert_eq!(preds[0].description, "fast");
    }

    #[test]
    fn flag_missing_evidence_finds_the_right_ones() {
        let text = "REQ: x\nACCEPT: proven TEST: t\nACCEPT: unproven\n";
        let preds = predicates_for_requirement(0, &req_of(text));
        let flagged = flag_missing_evidence(&preds);
        assert_eq!(flagged.len(), 1);
        assert_eq!(flagged[0].id, "P-0-2");
    }

    #[test]
    fn no_markers_yields_no_predicates() {
        let preds = predicates_for_requirement(7, &req_of("REQ: just prose, no conditions\n"));
        assert!(preds.is_empty());
    }

    #[test]
    fn empty_evidence_detail_means_no_plan() {
        let preds = predicates_for_requirement(0, &req_of("ACCEPT: x TEST:\n"));
        assert_eq!(preds.len(), 1);
        assert!(preds[0].evidence_plan.is_none());
    }

    #[test]
    fn bare_marker_with_no_condition_is_skipped() {
        let text = "REQ: x\nACCEPT:\nACCEPT: real condition\n";
        let preds = predicates_for_requirement(0, &req_of(text));
        assert_eq!(preds.len(), 1);
        assert_eq!(preds[0].id, "P-0-1");
        assert_eq!(preds[0].description, "real condition");
    }

    #[test]
    fn constructors_validate() {
        assert!(EvidencePlan::new(EvidenceKind::Test, "  ").is_err());
        assert!(TypedPredicate::new("", "desc", None).is_err());
        assert!(TypedPredicate::new("P-0-1", "", None).is_err());
        assert!(TypedPredicate::new("P-0-1", "desc", None).is_ok());
    }

    #[test]
    fn evidence_kind_serializes_as_names() {
        let json = serde_json::to_string(&EvidenceKind::Demonstration).unwrap();
        assert_eq!(json, "\"DEMONSTRATION\"");
    }
}
