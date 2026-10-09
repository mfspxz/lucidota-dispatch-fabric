//! T011 — INTERFACE CONTRACT (frozen by orchestrator).
//!
//! Parse a versioned master specification into typed requirements while
//! preserving source text and stable source locations.
//!
//! Worker S1: replace the `todo!()` body with a production implementation.
//! Public signatures are frozen; add private helpers freely.

use fabric_types::{Result, SpecId};
use serde::{Deserialize, Serialize};

/// Stable source location: 1-based inclusive line range in the spec text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceAnchor {
    pub line_start: u32,
    pub line_end: u32,
}

/// One requirement extracted from the master spec: original text plus anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    pub anchor: SourceAnchor,
    pub text: String,
}

/// A parsed master specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedSpec {
    pub revision: SpecId,
    /// sha256 of the exact source text parsed.
    pub source_digest: String,
    pub requirements: Vec<Requirement>,
}

/// Parse spec text into typed requirements.
///
/// Contract:
/// - `text` is split into requirements at blank-line-separated blocks whose
///   first line matches a requirement marker; the exact marker grammar is the
///   implementer's choice but must be documented and deterministic.
/// - Every requirement keeps its verbatim source text and a stable 1-based
///   line anchor.
/// - `source_digest` is `sha256:` + hex of the exact input text.
/// - Empty input yields an empty requirement list, not an error.
pub fn parse_spec(text: &str, revision: SpecId) -> Result<ParsedSpec> {
    let _ = (text, revision);
    todo!("T011: implement spec parsing")
}
