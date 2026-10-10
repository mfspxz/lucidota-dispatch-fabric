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

/// The only statement type this module ingests.
const STATEMENT_V1: &str = "https://in-toto.io/Statement/v1";
/// The only DSSE payload type this module ingests.
const DSSE_PAYLOAD_TYPE: &str = "application/vnd.in-toto+json";

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
    let v: serde_json::Value = serde_json::from_slice(json)
        .map_err(|e| AttestError::Malformed(format!("invalid JSON: {e}")))?;
    let obj = v
        .as_object()
        .ok_or_else(|| AttestError::Malformed("statement must be a JSON object".to_string()))?;

    let statement_type = obj
        .get("_type")
        .and_then(|t| t.as_str())
        .ok_or_else(|| AttestError::UnsupportedType("missing `_type`".to_string()))?;
    if statement_type != STATEMENT_V1 {
        return Err(AttestError::UnsupportedType(statement_type.to_string()));
    }

    let subjects = obj
        .get("subject")
        .map(parse_subjects)
        .transpose()?
        .unwrap_or_default();

    let predicate_type = obj
        .get("predicateType")
        .and_then(|t| t.as_str())
        .ok_or_else(|| AttestError::Malformed("missing `predicateType`".to_string()))?
        .to_string();

    let predicate = obj
        .get("predicate")
        .cloned()
        .ok_or_else(|| AttestError::Malformed("missing `predicate`".to_string()))?;

    Ok(IntotoStatement {
        statement_type: statement_type.to_string(),
        subjects,
        predicate_type,
        predicate,
    })
}

fn parse_subjects(v: &serde_json::Value) -> Result<Vec<Subject>, AttestError> {
    let arr = v
        .as_array()
        .ok_or_else(|| AttestError::Malformed("`subject` must be an array".to_string()))?;
    arr.iter()
        .enumerate()
        .map(|(i, s)| {
            let obj = s
                .as_object()
                .ok_or_else(|| AttestError::Malformed(format!("`subject[{i}]` must be an object")))?;
            let name = obj
                .get("name")
                .and_then(|n| n.as_str())
                .ok_or_else(|| AttestError::Malformed(format!("`subject[{i}]` missing `name`")))?
                .to_string();
            let digest_obj = obj
                .get("digest")
                .and_then(|d| d.as_object())
                .ok_or_else(|| AttestError::Malformed(format!("`subject[{i}]` missing `digest` object")))?;
            let mut digest = std::collections::HashMap::with_capacity(digest_obj.len());
            for (algo, hex) in digest_obj {
                let hex = hex.as_str().ok_or_else(|| {
                    AttestError::Malformed(format!("`subject[{i}]` digest `{algo}` must be a string"))
                })?;
                digest.insert(algo.to_lowercase(), hex.to_lowercase());
            }
            Ok(Subject { name, digest })
        })
        .collect()
}

/// Parse raw JSON bytes as a DSSE envelope. Does not verify signatures.
///
/// Rejects envelopes whose `payloadType` is not the in-toto JSON media type
/// (`UnsupportedType`) and envelopes with malformed base64 payloads
/// (`Malformed`). Key IDs are recorded verbatim; signature bytes are not
/// interpreted here.
pub fn parse_dsse_envelope(json: &[u8]) -> Result<DsseEnvelope, AttestError> {
    let v: serde_json::Value = serde_json::from_slice(json)
        .map_err(|e| AttestError::Malformed(format!("invalid JSON: {e}")))?;
    let obj = v
        .as_object()
        .ok_or_else(|| AttestError::Malformed("envelope must be a JSON object".to_string()))?;

    let payload_type = obj
        .get("payloadType")
        .and_then(|t| t.as_str())
        .ok_or_else(|| AttestError::Malformed("missing `payloadType`".to_string()))?;
    if payload_type != DSSE_PAYLOAD_TYPE {
        return Err(AttestError::UnsupportedType(payload_type.to_string()));
    }

    let payload_b64 = obj
        .get("payload")
        .and_then(|p| p.as_str())
        .ok_or_else(|| AttestError::Malformed("missing `payload`".to_string()))?;
    let payload = base64_decode(payload_b64)
        .map_err(|e| AttestError::Malformed(format!("bad base64 payload: {e}")))?;

    let mut key_ids = Vec::new();
    if let Some(sigs) = obj.get("signatures") {
        let arr = sigs
            .as_array()
            .ok_or_else(|| AttestError::Malformed("`signatures` must be an array".to_string()))?;
        for (i, sig) in arr.iter().enumerate() {
            let keyid = sig
                .get("keyid")
                .and_then(|k| k.as_str())
                .ok_or_else(|| AttestError::Malformed(format!("`signatures[{i}]` missing `keyid`")))?;
            key_ids.push(keyid.to_string());
        }
    }

    Ok(DsseEnvelope {
        payload_type: payload_type.to_string(),
        payload,
        key_ids,
    })
}

/// Content digest of the canonical statement bytes: `sha256:` + hex.
/// This is the evidence digest the statement is filed under.
///
/// Canonical form: the statement re-serialized with in-toto field names
/// (`_type`, `subject`, `predicateType`, `predicate`), object keys sorted
/// recursively, no whitespace. Field order in the input never affects the
/// digest.
pub fn statement_digest(stmt: &IntotoStatement) -> String {
    let v = serde_json::json!({
        "_type": stmt.statement_type,
        "predicate": stmt.predicate,
        "predicateType": stmt.predicate_type,
        "subject": stmt.subjects.iter().map(|s| {
            serde_json::json!({
                "digest": s.digest,
                "name": s.name,
            })
        }).collect::<Vec<_>>(),
    });
    let canonical = canonical_json(&v);
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(canonical.as_bytes());
    format!("sha256:{:x}", h.finalize())
}

/// Canonical JSON: object keys sorted recursively, no whitespace.
/// Scalars are serialized with serde_json for correct escaping.
fn canonical_json(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => serde_json::to_string(s).expect("string serializes"),
        serde_json::Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", inner.join(","))
        }
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).expect("key serializes"),
                        canonical_json(&map[*k])
                    )
                })
                .collect();
            format!("{{{}}}", inner.join(","))
        }
    }
}

/// Minimal standard-base64 decoder (RFC 4648 section 4, with padding).
///
/// Local implementation, no new dependency: the alphabet is 64 entries and
/// the whole decoder is ~40 lines. Documented choice — pulling in a crate
/// for one call site would add supply-chain surface for no benefit.
fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if !bytes.len().is_multiple_of(4) {
        return Err("length is not a multiple of 4".to_string());
    }
    let base = bytes.as_ptr() as usize;
    let end = base + bytes.len();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let chunk_start = chunk.as_ptr() as usize;
        let is_last = chunk_start + 4 >= end;
        let pad = chunk.iter().rev().take_while(|&&b| b == b'=').count();
        if pad > 2 {
            return Err("too much padding".to_string());
        }
        if pad > 0 && !is_last {
            return Err("padding before the final quantum".to_string());
        }
        let mut n: u32 = 0;
        for (i, &b) in chunk.iter().enumerate() {
            if b == b'=' {
                if i < 4 - pad {
                    return Err("padding in the middle of a quantum".to_string());
                }
                n <<= 6;
            } else {
                let d = val(b).ok_or_else(|| format!("invalid character: {}", b as char))?;
                n = (n << 6) | u32::from(d);
            }
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_statement() -> &'static str {
        r#"{
            "_type": "https://in-toto.io/Statement/v1",
            "subject": [
                {"name": "app.bin", "digest": {"sha256": "ABCDEF1234", "sha512": "9999"}}
            ],
            "predicateType": "https://slsa.dev/provenance/v1",
            "predicate": {"builder": {"id": "https://example.com/builder"}, "n": 42, "flag": true}
        }"#
    }

    #[test]
    fn parses_valid_statement() {
        let s = parse_statement(sample_statement().as_bytes()).unwrap();
        assert_eq!(s.statement_type, "https://in-toto.io/Statement/v1");
        assert_eq!(s.subjects.len(), 1);
        assert_eq!(s.subjects[0].name, "app.bin");
        // Digest algo + hex normalized to lowercase.
        assert_eq!(s.subjects[0].digest.get("sha256").unwrap(), "abcdef1234");
        assert_eq!(s.predicate_type, "https://slsa.dev/provenance/v1");
        assert_eq!(s.predicate["n"], 42);
    }

    #[test]
    fn rejects_wrong_type() {
        let bad = r#"{"_type": "https://example.com/Other/v9", "subject": [],
            "predicateType": "x", "predicate": {}}"#;
        let err = parse_statement(bad.as_bytes()).unwrap_err();
        assert!(matches!(err, AttestError::UnsupportedType(_)));
        assert!(err.to_string().contains("example.com/Other/v9"));
    }

    #[test]
    fn rejects_missing_type() {
        let bad = r#"{"subject": [], "predicateType": "x", "predicate": {}}"#;
        let err = parse_statement(bad.as_bytes()).unwrap_err();
        assert!(matches!(err, AttestError::UnsupportedType(_)));
    }

    #[test]
    fn rejects_garbage_json() {
        let err = parse_statement(b"not json").unwrap_err();
        assert!(matches!(err, AttestError::Malformed(_)));
    }

    #[test]
    fn rejects_missing_predicate() {
        let bad = r#"{"_type": "https://in-toto.io/Statement/v1", "subject": [],
            "predicateType": "x"}"#;
        let err = parse_statement(bad.as_bytes()).unwrap_err();
        assert!(matches!(err, AttestError::Malformed(_)));
    }

    #[test]
    fn statement_without_subjects_defaults_empty() {
        let ok = r#"{"_type": "https://in-toto.io/Statement/v1",
            "predicateType": "x", "predicate": {}}"#;
        let s = parse_statement(ok.as_bytes()).unwrap();
        assert!(s.subjects.is_empty());
    }

    fn sample_envelope() -> String {
        // payload = {"hello":"world"} — standard base64, two signatures.
        let payload_b64 = "eyJoZWxsbyI6IndvcmxkIn0=";
        format!(
            "{{\"payloadType\": \"application/vnd.in-toto+json\", \"payload\": \"{payload_b64}\", \
            \"signatures\": [{{\"keyid\": \"key-aaa\", \"sig\": \"c2lnMQ==\"}}, \
            {{\"keyid\": \"key-bbb\", \"sig\": \"c2lnMg==\"}}]}}"
        )
    }

    #[test]
    fn parses_dsse_envelope_and_records_key_ids() {
        let e = parse_dsse_envelope(sample_envelope().as_bytes()).unwrap();
        assert_eq!(e.payload_type, "application/vnd.in-toto+json");
        assert_eq!(e.payload, b"{\"hello\":\"world\"}");
        assert_eq!(e.key_ids, vec!["key-aaa".to_string(), "key-bbb".to_string()]);
    }

    #[test]
    fn envelope_without_signatures_yields_empty_key_ids() {
        let env = r#"{"payloadType": "application/vnd.in-toto+json",
            "payload": "e30=", "signatures": []}"#;
        let e = parse_dsse_envelope(env.as_bytes()).unwrap();
        assert!(e.key_ids.is_empty());
        assert_eq!(e.payload, b"{}");
    }

    #[test]
    fn rejects_malformed_base64() {
        let env = r#"{"payloadType": "application/vnd.in-toto+json",
            "payload": "!!!not-base64!!!", "signatures": []}"#;
        let err = parse_dsse_envelope(env.as_bytes()).unwrap_err();
        assert!(matches!(err, AttestError::Malformed(_)));
    }

    #[test]
    fn rejects_wrong_payload_type() {
        let env = r#"{"payloadType": "application/json",
            "payload": "e30=", "signatures": []}"#;
        let err = parse_dsse_envelope(env.as_bytes()).unwrap_err();
        assert!(matches!(err, AttestError::UnsupportedType(_)));
    }

    #[test]
    fn digest_is_stable_across_field_order() {
        // Same statement, shuffled top-level and nested key order.
        let shuffled = r#"{
            "predicate": {"n": 42, "flag": true, "builder": {"id": "https://example.com/builder"}},
            "predicateType": "https://slsa.dev/provenance/v1",
            "subject": [
                {"digest": {"sha512": "9999", "sha256": "ABCDEF1234"}, "name": "app.bin"}
            ],
            "_type": "https://in-toto.io/Statement/v1"
        }"#;
        let a = parse_statement(sample_statement().as_bytes()).unwrap();
        let b = parse_statement(shuffled.as_bytes()).unwrap();
        let da = statement_digest(&a);
        let db = statement_digest(&b);
        assert!(da.starts_with("sha256:"));
        assert_eq!(da.len(), 7 + 64);
        assert_eq!(da, db, "digest must not depend on input field order");
    }

    #[test]
    fn digest_changes_with_content() {
        let a = parse_statement(sample_statement().as_bytes()).unwrap();
        let mut b = a.clone();
        b.predicate_type = "https://example.com/other".to_string();
        assert_ne!(statement_digest(&a), statement_digest(&b));
    }

    #[test]
    fn digest_matches_canonical_bytes() {
        let s = parse_statement(
            r#"{"_type":"https://in-toto.io/Statement/v1","predicateType":"x","predicate":{}}"#.as_bytes(),
        )
        .unwrap();
        let d = statement_digest(&s);
        let expected_inner = r#"{"_type":"https://in-toto.io/Statement/v1","predicate":{},"predicateType":"x","subject":[]}"#;
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(expected_inner.as_bytes());
        assert_eq!(d, format!("sha256:{:x}", h.finalize()));
    }

    #[test]
    fn base64_known_vectors() {
        assert_eq!(base64_decode("").unwrap(), b"");
        assert_eq!(base64_decode("Zg==").unwrap(), b"f");
        assert_eq!(base64_decode("Zm9v").unwrap(), b"foo");
        assert_eq!(base64_decode("Zm9vYmFy").unwrap(), b"foobar");
        assert_eq!(base64_decode("Zm9vYmFyZA==").unwrap(), b"foobard");
        assert!(base64_decode("Zg=").is_err()); // length not multiple of 4
        assert!(base64_decode("====").is_err()); // all padding
        assert!(base64_decode("Zm9v=").is_err()); // length not multiple of 4
        assert!(base64_decode("ab!d").is_err()); // invalid char
        assert!(base64_decode("Zg==Zg==").is_err()); // padding before final quantum
    }

    #[test]
    fn envelope_payload_parses_as_statement() {
        // End-to-end: DSSE payload decodes to a parseable statement.
        let stmt_b64 = base64_encode(sample_statement().as_bytes());
        let env = format!(
            "{{\"payloadType\": \"application/vnd.in-toto+json\", \"payload\": \"{stmt_b64}\", \
            \"signatures\": [{{\"keyid\": \"k1\", \"sig\": \"eA==\"}}]}}"
        );
        let e = parse_dsse_envelope(env.as_bytes()).unwrap();
        let s = parse_statement(&e.payload).unwrap();
        assert_eq!(s.subjects[0].name, "app.bin");
        assert_eq!(e.key_ids, vec!["k1".to_string()]);
    }

    /// Tiny test-only base64 encoder (standard alphabet).
    fn base64_encode(input: &[u8]) -> String {
        const ALPHA: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in input.chunks(3) {
            let mut n: u32 = 0;
            for &b in chunk {
                n = (n << 8) | u32::from(b);
            }
            n <<= 8 * (3 - chunk.len());
            let pad = 3 - chunk.len();
            for i in 0..4 {
                if i < 4 - pad {
                    out.push(ALPHA[((n >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }
}
