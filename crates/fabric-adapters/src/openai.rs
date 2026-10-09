//! T033: OpenAI-compatible inference adapter.
//!
//! Configurable endpoint, model, extra headers, structured-output mode, and
//! usage parsing for the `/chat/completions` API shape. Implements the frozen
//! [`crate::traits::InferenceAdapter`] trait over [`crate::http::HttpTransport`].

use crate::errors::{AdapterError, AdapterErrorKind};
use crate::http::{CredentialResolver, HttpTransport, HttpTransportConfig};
use crate::traits::{
    CancellationToken, InferenceAdapter, InferenceRequest, InferenceResponse, InferenceUsage,
};
use async_trait::async_trait;
use fabric_types::{EndpointId, ModelId};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// How structured output is requested on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StructuredOutputMode {
    /// `response_format: {"type": "json_object"}` — portable across
    /// OpenAI-compatible providers.
    #[default]
    JsonObject,
}

/// Adapter configuration. Endpoint and model are required; the rest has
/// safe defaults.
#[derive(Debug, Clone)]
pub struct OpenAiConfig {
    pub base_url: String,
    pub model: ModelId,
    pub extra_headers: Vec<(String, String)>,
    pub structured_output_mode: StructuredOutputMode,
    pub transport: HttpTransportConfig,
}

impl OpenAiConfig {
    /// `base_url` must be an http(s) URL; trailing slashes are trimmed.
    pub fn new(base_url: impl Into<String>, model: ModelId) -> Result<Self, AdapterError> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err(AdapterError::fatal(
                AdapterErrorKind::Unknown,
                "base_url must start with http:// or https://",
            ));
        }
        Ok(Self {
            base_url,
            model,
            extra_headers: Vec::new(),
            structured_output_mode: StructuredOutputMode::JsonObject,
            transport: HttpTransportConfig::default(),
        })
    }

    /// Add a static header. `authorization` is rejected: credentials enter
    /// only through the per-call bearer path.
    pub fn extra_header(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, AdapterError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(AdapterError::fatal(
                AdapterErrorKind::Unknown,
                "header name must be non-empty",
            ));
        }
        if name.eq_ignore_ascii_case("authorization") {
            return Err(AdapterError::fatal(
                AdapterErrorKind::Unknown,
                "authorization header is managed by the credential path",
            ));
        }
        self.extra_headers.push((name, value.into()));
        Ok(self)
    }
}

/// OpenAI-compatible chat-completions adapter.
pub struct OpenAiAdapter {
    endpoint_id: EndpointId,
    config: OpenAiConfig,
    transport: HttpTransport,
    credentials: Arc<dyn CredentialResolver>,
}

impl OpenAiAdapter {
    pub fn new(
        endpoint_id: EndpointId,
        config: OpenAiConfig,
        credentials: Arc<dyn CredentialResolver>,
    ) -> Result<Self, AdapterError> {
        let transport = HttpTransport::new(config.transport.clone())?;
        Ok(Self {
            endpoint_id,
            config,
            transport,
            credentials,
        })
    }

    pub fn config(&self) -> &OpenAiConfig {
        &self.config
    }

    /// Build the chat-completions request body. Public for testing and for
    /// provider-specific adapters that share this wire shape.
    pub fn request_body(&self, req: &InferenceRequest) -> serde_json::Value {
        let mut body = json!({
            "model": self.config.model.as_str(),
            "messages": [{"role": "user", "content": req.prompt}],
        });
        if let Some(n) = req.max_tokens {
            body["max_tokens"] = json!(n);
        }
        if req.structured_output {
            body["response_format"] = match self.config.structured_output_mode {
                StructuredOutputMode::JsonObject => json!({"type": "json_object"}),
            };
        }
        body
    }

    /// Parse a chat-completions response body.
    ///
    /// Returns `(request_id, usage, finish_reason)`. The full body is kept as
    /// `raw_body` on the response; message-content extraction is T038's job.
    /// A provider error envelope (some proxies return 200 + `error`) is
    /// mapped to a typed error here.
    pub fn parse_response(
        body_text: &str,
        header_request_id: Option<&str>,
    ) -> Result<(String, Option<InferenceUsage>, Option<String>), AdapterError> {
        let v: serde_json::Value = serde_json::from_str(body_text).map_err(|e| {
            AdapterError::fatal(
                AdapterErrorKind::MalformedResponse,
                format!("invalid chat-completions JSON: {e}"),
            )
        })?;
        if let Some(err) = v.get("error") {
            return Err(map_provider_error(err));
        }
        let request_id = v
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| header_request_id.map(|s| s.to_string()))
            .unwrap_or_else(|| "unknown".to_string());
        let usage = v.get("usage").and_then(|u| {
            let input = u.get("prompt_tokens")?.as_u64()?;
            let output = u.get("completion_tokens")?.as_u64()?;
            Some(InferenceUsage {
                input_tokens: input,
                output_tokens: output,
            })
        });
        let finish_reason = v
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason"))
            .and_then(|f| f.as_str())
            .map(|s| s.to_string());
        Ok((request_id, usage, finish_reason))
    }
}

/// Map a provider `error` envelope to a typed error. Quota/rate-limit
/// signals are retryable; auth problems are fatal.
fn map_provider_error(err: &serde_json::Value) -> AdapterError {
    let message = err
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("provider error");
    let code = err.get("code").and_then(|c| c.as_str()).unwrap_or("");
    let typ = err.get("type").and_then(|t| t.as_str()).unwrap_or("");
    // Bounded by the AdapterError constructors.
    let detail = format!("{typ}/{code}: {message}");
    if code.contains("quota")
        || code.contains("rate_limit")
        || typ.contains("rate_limit")
        || typ.contains("quota")
    {
        AdapterError::retryable(AdapterErrorKind::Quota, detail)
    } else if code.contains("invalid_api_key") || typ.contains("auth") {
        AdapterError::fatal(AdapterErrorKind::Auth, detail)
    } else {
        AdapterError::fatal(AdapterErrorKind::Unknown, detail)
    }
}

#[async_trait]
impl InferenceAdapter for OpenAiAdapter {
    async fn infer(
        &self,
        req: InferenceRequest,
        cancel: &CancellationToken,
    ) -> Result<InferenceResponse, AdapterError> {
        if req.timeout_ms == 0 {
            return Err(AdapterError::fatal(
                AdapterErrorKind::Unknown,
                "timeout_ms must be positive",
            ));
        }
        if cancel.is_cancelled() {
            return Err(AdapterError::fatal(
                AdapterErrorKind::Cancelled,
                "cancelled before infer",
            ));
        }
        let bearer = match &req.credential {
            Some(r) => Some(self.credentials.bearer_token(r)?),
            None => None,
        };
        let url = format!("{}/chat/completions", self.config.base_url);
        let body = self.request_body(&req);
        let headers: Vec<(&str, &str)> = self
            .config
            .extra_headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let deadline = Duration::from_millis(req.timeout_ms);
        let http_resp = tokio::time::timeout(
            deadline,
            self.transport
                .post_json(&url, bearer.as_deref(), &headers, &body, cancel),
        )
        .await
        .map_err(|_| {
            AdapterError::retryable(
                AdapterErrorKind::Timeout,
                format!("infer exceeded {}ms", req.timeout_ms),
            )
        })??;
        let body_text = http_resp.body_text();
        let (request_id, usage, finish_reason) =
            Self::parse_response(&body_text, http_resp.request_id.as_deref())?;
        Ok(InferenceResponse {
            request_id,
            raw_body: body_text,
            usage,
            finish_reason,
        })
    }

    fn endpoint_id(&self) -> &EndpointId {
        &self.endpoint_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::test_support::{FakeSpec, fake_server};
    use fabric_types::SecretRef;

    struct TestResolver;
    impl CredentialResolver for TestResolver {
        fn bearer_token(&self, _r: &SecretRef) -> Result<String, AdapterError> {
            Ok("test-token".to_string())
        }
    }

    fn resolver() -> Arc<dyn CredentialResolver> {
        Arc::new(TestResolver)
    }

    fn adapter_for(url: &str) -> OpenAiAdapter {
        let config = OpenAiConfig::new(url, ModelId::new("test-model").unwrap()).unwrap();
        OpenAiAdapter::new(EndpointId::new("ep-openai").unwrap(), config, resolver()).unwrap()
    }

    fn sample_request() -> InferenceRequest {
        InferenceRequest {
            prompt: "say hi".to_string(),
            model: ModelId::new("test-model").unwrap(),
            max_tokens: Some(64),
            structured_output: false,
            timeout_ms: 10_000,
            credential: Some(SecretRef::new("custom.test").unwrap()),
        }
    }

    #[test]
    fn config_rejects_non_http_url() {
        assert!(OpenAiConfig::new("ftp://x", ModelId::new("m").unwrap()).is_err());
        assert!(OpenAiConfig::new("https://api.example.com/", ModelId::new("m").unwrap()).is_ok());
    }

    #[test]
    fn extra_header_rejects_authorization() {
        let cfg = || OpenAiConfig::new("https://api.example.com", ModelId::new("m").unwrap()).unwrap();
        assert!(cfg().extra_header("Authorization", "x").is_err());
        assert!(cfg().extra_header("AUTHORIZATION", "x").is_err());
        assert!(cfg().extra_header("x-custom", "v").is_ok());
    }

    #[test]
    fn request_body_shape() {
        let a = adapter_for("https://api.example.com");
        let mut req = sample_request();
        let body = a.request_body(&req);
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "say hi");
        assert_eq!(body["max_tokens"], 64);
        assert!(body.get("response_format").is_none());

        req.structured_output = true;
        req.max_tokens = None;
        let body = a.request_body(&req);
        assert_eq!(body["response_format"]["type"], "json_object");
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn parse_response_extracts_fields() {
        let text = r#"{
            "id": "chatcmpl-abc",
            "choices": [{"message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 3, "total_tokens": 13}
        }"#;
        let (id, usage, finish) = OpenAiAdapter::parse_response(text, None).unwrap();
        assert_eq!(id, "chatcmpl-abc");
        assert_eq!(
            usage,
            Some(InferenceUsage {
                input_tokens: 10,
                output_tokens: 3
            })
        );
        assert_eq!(finish.as_deref(), Some("stop"));
    }

    #[test]
    fn parse_response_falls_back_to_header_request_id() {
        let text = r#"{"choices": [], "usage": {}}"#;
        let (id, usage, _) = OpenAiAdapter::parse_response(text, Some("hdr-1")).unwrap();
        assert_eq!(id, "hdr-1");
        assert_eq!(usage, None);
    }

    #[test]
    fn parse_response_maps_error_envelope() {
        let quota = r#"{"error": {"message": "slow down", "type": "rate_limit_error", "code": "rate_limit_exceeded"}}"#;
        let err = OpenAiAdapter::parse_response(quota, None).unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::Quota);
        assert!(err.is_retryable());

        let auth = r#"{"error": {"message": "bad key", "type": "auth_error", "code": "invalid_api_key"}}"#;
        let err = OpenAiAdapter::parse_response(auth, None).unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::Auth);
        assert!(!err.is_retryable());

        let bad = OpenAiAdapter::parse_response("not json", None).unwrap_err();
        assert_eq!(bad.kind, AdapterErrorKind::MalformedResponse);
    }

    #[tokio::test]
    async fn infer_hits_chat_completions_with_bearer() {
        let body = br#"{
            "id": "chatcmpl-1",
            "choices": [{"finish_reason": "stop", "message": {"content": "hello"}}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}
        }"#
        .to_vec();
        let (url, rx) = fake_server(FakeSpec::ok(body)).await;
        let a = adapter_for(&url);
        let resp = a.infer(sample_request(), &CancellationToken::new()).await.unwrap();
        assert_eq!(resp.request_id, "chatcmpl-1");
        assert_eq!(
            resp.usage,
            Some(InferenceUsage {
                input_tokens: 5,
                output_tokens: 2
            })
        );
        assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
        assert!(resp.raw_body.contains("chatcmpl-1"));

        let raw = String::from_utf8(rx.recv().unwrap()).unwrap();
        assert!(raw.starts_with("POST /chat/completions "));
        assert!(raw.to_ascii_lowercase().contains("authorization: bearer test-token"));
        assert!(raw.contains(r#""model":"test-model""#));
    }

    #[tokio::test]
    async fn infer_without_credential_sends_no_auth_header() {
        let (url, rx) = fake_server(FakeSpec::ok(br#"{"id":"x","choices":[]}"#.to_vec())).await;
        let a = adapter_for(&url);
        let mut req = sample_request();
        req.credential = None;
        a.infer(req, &CancellationToken::new()).await.unwrap();
        let raw = String::from_utf8(rx.recv().unwrap()).unwrap();
        assert!(!raw.to_ascii_lowercase().contains("authorization:"));
    }

    #[tokio::test]
    async fn infer_timeout_is_typed() {
        let (url, _) = fake_server(FakeSpec {
            delay_ms: 1500,
            ..FakeSpec::ok(br#"{}"#.to_vec())
        })
        .await;
        let a = adapter_for(&url);
        let mut req = sample_request();
        req.timeout_ms = 150;
        let err = a.infer(req, &CancellationToken::new()).await.unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::Timeout);
        assert!(err.is_retryable());
    }
}
