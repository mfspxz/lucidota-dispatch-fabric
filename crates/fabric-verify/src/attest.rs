//! INT-01 — INTERFACE CONTRACT (frozen by orchestrator).
//!
//! Ingest in-toto attestations as evidence. Parses in-toto Statements
//! (`https://in-toto.io/Statement/v1`) and DSSE envelopes
//! (`application/vnd.in-toto+json`), extracting subjects, predicate types,
//! and predicates. Signature verification is NOT done here — signatures are
//! recorded as present/absent, and cryptographic verification is delegated
//! to a registered verifier command (e.g. `cosign verify`) via the verifier
//! registry. This module parses shape; verifiers establish trust.
//!
//! Worker INT1: replace the `todo!()` bodies with production implementations.
//! Public signatures are frozen; add private helpers freely.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AttestError {
    #[error("malformed statement: {0}")]
    Malformed(String),
    #[error("unsupported statement type: {0}")]
    UnsupportedType(String),
}

/// A parsed in-toto Statement (v1), payload only (no envelope).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntotoStatement {
    /// Full `_type` URI, e.g. `https://in-toto.io/Statement/v1`.
    pub statement_type: String,
    pub subjects: Vec<Subject>,
    pub predicate_type: String,
    pub predicate: serde_json::Value,
}

/// One subject of a statement: a named artifact with digests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subject {
    pub name: String,
    /// Algorithm -> lowercase hex, e.g. {"sha256": "abc..."}.
    pub digest: std::collections::HashMap<String, String>,
}

/// A parsed DSSE envelope carrying an in-toto payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DsseEnvelope {
    pub payload_type: String,
    /// Raw payload bytes (base64-decoded).
    pub payload: Vec<u8>,
    /// Key IDs that signed; signatures are recorded, not verified here.
    pub key_ids: Vec<String>,
}

/// Parse raw JSON bytes as an in-toto Statement. Rejects wrong `_type`.
pub fn parse_statement(json: &[u8]) -> Result<IntotoStatement, AttestError> {
    let _ = json;
    todo!("INT-01: implement in-toto statement parsing")
}

/// Parse raw JSON bytes as a DSSE envelope. Does not verify signatures.
pub fn parse_dsse_envelope(json: &[u8]) -> Result<DsseEnvelope, AttestError> {
    let _ = json;
    todo!("INT-01: implement DSSE envelope parsing")
}

/// Content digest of the canonical statement bytes: `sha256:` + hex.
/// This is the evidence digest the statement is filed under.
pub fn statement_digest(stmt: &IntotoStatement) -> String {
    let _ = stmt;
    todo!("INT-01: implement statement digest")
}
