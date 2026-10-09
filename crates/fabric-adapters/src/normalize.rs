//! T038: normalize provider responses.
//!
//! Turns a raw [`InferenceResponse`](crate::traits::InferenceResponse) into a
//! provider-neutral [`NormalizedResponse`]: text, optional structured payload,
//! usage, finish reason, request ID, and a content-addressed
//! [`ArtifactRef`](fabric_types::ArtifactRef) over the canonical normalized
//! payload (sha256 digest, so identical normalizations deduplicate).
//!
//! Unknowns stay unknown: unreported token counts normalize to zero and are
//! documented as "not reported", never invented.

use crate::errors::{AdapterError, AdapterErrorKind};
use crate::traits::{InferenceResponse, InferenceUsage};
use fabric_types::ArtifactRef;
use sha2::{Digest, Sha256};

/// T038: normalized finish reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
    ContentFilter,
    Other(String),
}

impl FinishReason {
    /// Parse a provider finish-reason string. Unknown strings become
    /// [`FinishReason::Other`], never an error; a missing reason maps to
    /// `Other("unknown")`.
    pub fn parse(s: Option<&str>) -> Self {
        match s {
            Some("stop") => Self::Stop,
            Some("length") => Self::Length,
            Some("content_filter") => Self::ContentFilter,
            Some(other) => Self::Other(other.to_string()),
            None => Self::Other("unknown".to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Stop => "stop",
            Self::Length => "length",
            Self::ContentFilter => "content_filter",
            Self::Other(s) => s.as_str(),
        }
    }
}

impl std::fmt::Display for FinishReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// T038: provider-neutral normalized response.
#[derive(Debug, Clone)]
pub struct NormalizedResponse {
    /// The model's text output.
    pub text: String,
    /// The text parsed as JSON when it is valid JSON, else `None`.
    pub structured: Option<serde_json::Value>,
    /// Token usage; zeros mean "not reported by the provider".
    pub usage: InferenceUsage,
    pub finish_reason: FinishReason,
    pub request_id: String,
    /// Content-addressed reference to the canonical normalized payload.
    pub artifact: ArtifactRef,
}

/// Normalize a raw provider response.
///
/// Contract:
/// - `raw_body` must be a JSON chat-completion-like object with at least one
///   choice carrying text content; otherwise [`AdapterErrorKind::MalformedResponse`].
/// - `content` may be a string or an array of `{type:"text", text:...}` blocks.
/// - Token counts prefer the typed `resp.usage`, then the body's `usage`
///   object (`prompt_tokens`/`completion_tokens` or `input_tokens`/`output_tokens`);
///   absent counts become 0 ("not reported").
/// - The artifact digest is `sha256` over a canonical JSON payload with
///   deterministic key order: `finish_reason`, `request_id`, `text`, `usage`.
pub fn normalize_response(
    resp: &InferenceResponse,
    provenance: &str,
) -> Result<NormalizedResponse, AdapterError> {
    let body: serde_json::Value = serde_json::from_str(&resp.raw_body).map_err(|e| {
        AdapterError::fatal(
            AdapterErrorKind::MalformedResponse,
            format!("response body is not JSON: {e}"),
        )
    })?;

    let text = extract_text(&body)?;
    let structured = serde_json::from_str::<serde_json::Value>(&text).ok();
    let usage = extract_usage(resp, &body);
    let finish_reason = FinishReason::parse(
        resp.finish_reason
            .as_deref()
            .or_else(|| body_choice_str(&body, "finish_reason")),
    );
    let request_id = if !resp.request_id.trim().is_empty() {
        resp.request_id.clone()
    } else if let Some(id) = body.get("id").and_then(|v| v.as_str()) {
        id.to_string()
    } else {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("normalized-{nanos}")
    };

    // Canonical payload: serde_json::Map is a BTreeMap (preserve_order off),
    // so key order is deterministic.
    let mut payload = serde_json::Map::new();
    payload.insert(
        "finish_reason".to_string(),
        serde_json::Value::String(finish_reason.as_str().to_string()),
    );
    payload.insert(
        "request_id".to_string(),
        serde_json::Value::String(request_id.clone()),
    );
    payload.insert("text".to_string(), serde_json::Value::String(text.clone()));
    let mut usage_map = serde_json::Map::new();
    usage_map.insert(
        "input_tokens".to_string(),
        serde_json::Value::Number(usage.input_tokens.into()),
    );
    usage_map.insert(
        "output_tokens".to_string(),
        serde_json::Value::Number(usage.output_tokens.into()),
    );
    payload.insert("usage".to_string(), serde_json::Value::Object(usage_map));
    let canonical = serde_json::to_string(&serde_json::Value::Object(payload))
        .expect("canonical payload serialization cannot fail");

    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let digest = format!("sha256:{:x}", hasher.finalize());
    let artifact = ArtifactRef::new(digest, "application/json", canonical.len() as u64, provenance)
        .map_err(|e| {
            AdapterError::fatal(
                AdapterErrorKind::MalformedResponse,
                format!("failed to build artifact ref: {e}"),
            )
        })?;

    Ok(NormalizedResponse {
        text,
        structured,
        usage,
        finish_reason,
        request_id,
        artifact,
    })
}

/// Extract text from `choices[0].message.content`: a string, or an array of
/// `{type:"text", text:...}` content blocks concatenated in order.
fn extract_text(body: &serde_json::Value) -> Result<String, AdapterError> {
    let malformed = |why: &str| {
        AdapterError::fatal(
            AdapterErrorKind::MalformedResponse,
            format!("no text content in response: {why}"),
        )
    };
    let content = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .ok_or_else(|| malformed("missing choices[0].message.content"))?;

    match content {
        serde_json::Value::String(s) => {
            if s.is_empty() {
                Err(malformed("content is empty"))
            } else {
                Ok(s.clone())
            }
        }
        serde_json::Value::Array(blocks) => {
            let mut out = String::new();
            for b in blocks {
                if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                        out.push_str(t);
                    }
                }
            }
            if out.is_empty() {
                Err(malformed("no text blocks in content array"))
            } else {
                Ok(out)
            }
        }
        _ => Err(malformed("content is neither string nor block array")),
    }
}

/// Token usage: typed response first, then body `usage` object, else zeros
/// ("not reported").
fn extract_usage(resp: &InferenceResponse, body: &serde_json::Value) -> InferenceUsage {
    if let Some(u) = resp.usage {
        return u;
    }
    let usage = body.get("usage");
    let input = usage
        .and_then(|u| {
            u.get("prompt_tokens")
                .or_else(|| u.get("input_tokens"))
                .and_then(|v| v.as_u64())
        })
        .unwrap_or(0);
    let output = usage
        .and_then(|u| {
            u.get("completion_tokens")
                .or_else(|| u.get("output_tokens"))
                .and_then(|v| v.as_u64())
        })
        .unwrap_or(0);
    InferenceUsage {
        input_tokens: input,
        output_tokens: output,
    }
}

/// Read a string field off `choices[0]`.
fn body_choice_str<'a>(body: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    body.get("choices")?
        .as_array()?
        .first()?
        .get(field)?
        .as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(raw: &str) -> InferenceResponse {
        InferenceResponse {
            request_id: "req-1".to_string(),
            raw_body: raw.to_string(),
            usage: None,
            finish_reason: None,
        }
    }

    const OPENAI_OK: &str = r#"{
        "id": "chatcmpl-abc",
        "choices": [{"message": {"content": "hello world"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 2}
    }"#;

    #[test]
    fn normalizes_openai_body() {
        let n = normalize_response(&resp(OPENAI_OK), "unit-test").unwrap();
        assert_eq!(n.text, "hello world");
        assert_eq!(n.finish_reason, FinishReason::Stop);
        assert_eq!(n.request_id, "req-1");
        assert_eq!(n.usage.input_tokens, 10);
        assert_eq!(n.usage.output_tokens, 2);
        assert!(n.structured.is_none());
        assert_eq!(n.artifact.media_type, "application/json");
        assert!(n.artifact.digest.starts_with("sha256:"));
        assert_eq!(n.artifact.algorithm(), "sha256");
    }

    #[test]
    fn json_text_becomes_structured_payload() {
        let raw = r#"{"id":"c","choices":[{"message":{"content":"{\"a\":1}"},"finish_reason":"stop"}]}"#;
        let n = normalize_response(&resp(raw), "unit-test").unwrap();
        assert_eq!(n.structured, Some(serde_json::json!({"a": 1})));
    }

    #[test]
    fn content_blocks_concatenate() {
        let raw = r#"{"id":"c","choices":[{"message":{"content":[
            {"type":"text","text":"foo "},
            {"type":"image","url":"http://x"},
            {"type":"text","text":"bar"}
        ]},"finish_reason":"length"}]}"#;
        let n = normalize_response(&resp(raw), "unit-test").unwrap();
        assert_eq!(n.text, "foo bar");
        assert_eq!(n.finish_reason, FinishReason::Length);
    }

    #[test]
    fn typed_usage_wins_over_body() {
        let mut r = resp(OPENAI_OK);
        r.usage = Some(InferenceUsage {
            input_tokens: 99,
            output_tokens: 1,
        });
        let n = normalize_response(&r, "unit-test").unwrap();
        assert_eq!(n.usage.input_tokens, 99);
    }

    #[test]
    fn missing_usage_is_zero_not_invented() {
        let raw = r#"{"id":"c","choices":[{"message":{"content":"x"},"finish_reason":"stop"}]}"#;
        let n = normalize_response(&resp(raw), "unit-test").unwrap();
        assert_eq!(n.usage.input_tokens, 0);
        assert_eq!(n.usage.output_tokens, 0);
    }

    #[test]
    fn alternate_usage_keys_supported() {
        let raw = r#"{"id":"c","choices":[{"message":{"content":"x"}}],"usage":{"input_tokens":7,"output_tokens":3}}"#;
        let n = normalize_response(&resp(raw), "unit-test").unwrap();
        assert_eq!(n.usage.input_tokens, 7);
        assert_eq!(n.usage.output_tokens, 3);
    }

    #[test]
    fn finish_reason_mapping() {
        assert_eq!(FinishReason::parse(Some("stop")), FinishReason::Stop);
        assert_eq!(FinishReason::parse(Some("length")), FinishReason::Length);
        assert_eq!(
            FinishReason::parse(Some("content_filter")),
            FinishReason::ContentFilter
        );
        assert_eq!(
            FinishReason::parse(Some("tool_calls")),
            FinishReason::Other("tool_calls".to_string())
        );
        assert_eq!(
            FinishReason::parse(None),
            FinishReason::Other("unknown".to_string())
        );
    }

    #[test]
    fn typed_finish_reason_wins() {
        let mut r = resp(OPENAI_OK);
        r.finish_reason = Some("length".to_string());
        let n = normalize_response(&r, "unit-test").unwrap();
        assert_eq!(n.finish_reason, FinishReason::Length);
    }

    #[test]
    fn request_id_falls_back_to_body_then_synthetic() {
        let mut r = resp(OPENAI_OK);
        r.request_id = String::new();
        let n = normalize_response(&r, "unit-test").unwrap();
        assert_eq!(n.request_id, "chatcmpl-abc");

        let raw = r#"{"choices":[{"message":{"content":"x"}}]}"#;
        let mut r2 = resp(raw);
        r2.request_id = String::new();
        let n = normalize_response(&r2, "unit-test").unwrap();
        assert!(n.request_id.starts_with("normalized-"), "{}", n.request_id);
    }

    #[test]
    fn digest_is_deterministic_and_content_addressed() {
        let a = normalize_response(&resp(OPENAI_OK), "prov").unwrap();
        let b = normalize_response(&resp(OPENAI_OK), "prov").unwrap();
        assert_eq!(a.artifact.digest, b.artifact.digest);

        // Recompute independently: digest must equal sha256 of the canonical payload.
        let mut payload = serde_json::Map::new();
        payload.insert("finish_reason".to_string(), serde_json::json!("stop"));
        payload.insert("request_id".to_string(), serde_json::json!("req-1"));
        payload.insert("text".to_string(), serde_json::json!("hello world"));
        payload.insert("usage".to_string(), serde_json::json!({"input_tokens": 10, "output_tokens": 2}));
        let canonical = serde_json::to_string(&serde_json::Value::Object(payload)).unwrap();
        let mut h = Sha256::new();
        h.update(canonical.as_bytes());
        assert_eq!(a.artifact.digest, format!("sha256:{:x}", h.finalize()));
        assert_eq!(a.artifact.size_bytes, canonical.len() as u64);
    }

    #[test]
    fn malformed_bodies_rejected() {
        for raw in [
            "not json",
            r#"{"id":"c"}"#,
            r#"{"choices":[]}"#,
            r#"{"choices":[{"message":{}}]}"#,
            r#"{"choices":[{"message":{"content":null}}]}"#,
            r#"{"choices":[{"message":{"content":""}}]}"#,
        ] {
            let e = normalize_response(&resp(raw), "unit-test").unwrap_err();
            assert_eq!(e.kind, AdapterErrorKind::MalformedResponse, "{raw}");
            assert!(!e.is_retryable());
        }
    }
}
