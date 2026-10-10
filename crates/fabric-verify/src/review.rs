//! T097 — independent review dispatch with mechanical separation of duties.
//!
//! Two properties are enforced, not assumed:
//!
//! 1. The review request sent to the reviewer must NOT contain the
//!    implementer's artifact digest (the reviewer's judgment is never keyed
//!    on the implementer's artifact identity).
//! 2. The reviewer identity must differ from the implementer identity.
//!
//! Violation of either is a typed `FabricError::PolicyDenied`.

use fabric_types::{FabricError, Result, TaskId};
use serde::{Deserialize, Serialize};

/// A request for independent review of one task attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub task_id: TaskId,
    /// Stable identity of the implementer (never its secret material).
    pub implementer_id: String,
    /// Digest of the implementer's artifact. Held here ONLY so the dispatch
    /// layer can prove it was excluded from the review payload.
    pub implementer_artifact_digest: String,
    pub requirement_id: String,
}

/// An eligible reviewer endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reviewer {
    /// Stable reviewer identity. Must differ from any implementer it reviews.
    pub id: String,
    /// Endpoint the review request is sent to.
    pub endpoint: String,
}

/// The payload actually sent to the reviewer. Note the deliberate absence of
/// the implementer's artifact digest: it is not a field, so it cannot leak.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewPayload {
    pub task_id: TaskId,
    pub requirement_id: String,
    pub reviewer_id: String,
}

/// A completed review assignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewAssignment {
    pub reviewer_id: String,
    pub endpoint: String,
    pub payload: ReviewPayload,
}

impl ReviewRequest {
    fn validate(&self) -> Result<()> {
        if self.implementer_id.is_empty() {
            return Err(FabricError::InvalidInput {
                field: "review.implementer_id",
                reason: "empty".to_string(),
            });
        }
        if self.requirement_id.is_empty() {
            return Err(FabricError::InvalidInput {
                field: "review.requirement_id",
                reason: "empty".to_string(),
            });
        }
        Ok(())
    }
}

/// Dispatch independent review to the first eligible candidate.
///
/// Mechanical separation of duties:
/// - `candidates` must contain at least one reviewer whose `id` differs from
///   the request's `implementer_id`, otherwise `PolicyDenied`.
/// - The returned payload never carries `implementer_artifact_digest` — the
///   type cannot express it.
///
/// Candidates are tried in order; the first identity-distinct one wins.
pub fn dispatch_review(request: &ReviewRequest, candidates: &[Reviewer]) -> Result<ReviewAssignment> {
    request.validate()?;
    for c in candidates {
        if c.id.is_empty() || c.endpoint.is_empty() {
            continue;
        }
        if c.id == request.implementer_id {
            return Err(FabricError::PolicyDenied(format!(
                "reviewer '{}' is the implementer: separation of duties",
                c.id
            )));
        }
        return Ok(ReviewAssignment {
            reviewer_id: c.id.clone(),
            endpoint: c.endpoint.clone(),
            payload: ReviewPayload {
                task_id: request.task_id.clone(),
                requirement_id: request.requirement_id.clone(),
                reviewer_id: c.id.clone(),
            },
        });
    }
    Err(FabricError::PolicyDenied(
        "no eligible independent reviewer available".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(implementer: &str) -> ReviewRequest {
        ReviewRequest {
            task_id: TaskId::new("T-1").unwrap(),
            implementer_id: implementer.to_string(),
            implementer_artifact_digest: "sha256:deadbeef".to_string(),
            requirement_id: "REQ-1".to_string(),
        }
    }

    fn reviewer(id: &str) -> Reviewer {
        Reviewer {
            id: id.to_string(),
            endpoint: "ep".to_string(),
        }
    }

    #[test]
    fn same_identity_reviewer_is_policy_denied() {
        let err = dispatch_review(&req("alice"), &[reviewer("alice")]).unwrap_err();
        assert!(matches!(err, FabricError::PolicyDenied(_)));
    }

    #[test]
    fn first_distinct_candidate_wins() {
        let a = dispatch_review(&req("alice"), &[reviewer("bob"), reviewer("carol")]).unwrap();
        assert_eq!(a.reviewer_id, "bob");
    }

    #[test]
    fn payload_cannot_carry_implementer_digest() {
        let a = dispatch_review(&req("alice"), &[reviewer("bob")]).unwrap();
        let ser = serde_json::to_string(&a.payload).unwrap();
        assert!(!ser.contains("deadbeef"), "implementer digest must not leak into payload");
        assert!(!ser.contains("artifact"));
    }

    #[test]
    fn no_candidates_is_policy_denied() {
        let err = dispatch_review(&req("alice"), &[]).unwrap_err();
        assert!(matches!(err, FabricError::PolicyDenied(_)));
    }

    #[test]
    fn empty_implementer_id_is_invalid_input() {
        let err = dispatch_review(&req(""), &[reviewer("bob")]).unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }));
    }
}
