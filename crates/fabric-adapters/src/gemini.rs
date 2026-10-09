//! T035: Gemini inference adapter.
//!
//! Provider-specific request/response mapping for the Gemini `generateContent`
//! REST API, with typed quota errors: HTTP 429 or a `RESOURCE_EXHAUSTED`
//! error status maps to [`AdapterErrorKind::Quota`] (retryable), which
//! [`AdapterError::to_fabric`] turns into `FabricError::QuotaExhausted`.
//!
//! The API key travels in the `x-goog-api-key` header, never in the URL, so
//! it cannot leak through redacted URL diagnostics.

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

/// Documented Gemini base URL.
pub const GEMINI_DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Adapter configuration.
#[derive(Debug, Clone)]
pub struct GeminiConfig {
    pub model: ModelId,
    /// Override the base URL. Defaults to [`GEMINI_DEFAULT_BASE_URL`].
    pub base_url: Option<String>,
    pub transport: HttpTransportConfig,
}

impl GeminiConfig {
    pub fn new(model: ModelId) -> Self {
        Self {
            model,
            base_url: None,
            transport: HttpTransportConfig::default(),
        }
    }

    fn base_url(&self) -> &str {
        self.base_url.as_deref().unwrap_or(GEMINI_DEFAULT_BASE_URL)
    }
}

/// Gemini `generateContent` adapter.
pub struct GeminiAdapter {
    endpoint_id: EndpointId,
    config: GeminiConfig,
    transport: HttpTransport,
    credentials: Arc<dyn CredentialResolver>,
}

impl GeminiAdapter {
    pub fn new(
        endpoint_id: EndpointId,
        config: GeminiConfig,
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

    pub fn config(&self) -> &GeminiConfig {
        &self.config
    }

    /// Build the `generateContent` request body. Public for testing.
    pub fn request_body(&self, req: &InferenceRequest) -> serde_json::Value {
        let mut generation_config = json!({});
        if let Some(n) = req.max_tokens {
            generation_config["maxOutputTokens"] = json!(n);
        }
        if req.structured_output {
            generation_config["responseMimeType"] = json!("application/json");
        }
        json!({
            "contents": [{"parts": [{"text": req.prompt}]}],
            "generationConfig": generation_config,
        })
    }

    /// Parse a `generateContent` response body.
    ///
    /// Returns `(request_id, usage, finish_reason)`. The full body is kept as
    /// `raw_body`; candidate-text extraction is T038's job. A provider error
    /// envelope is mapped to a typed error with quota called out explicitly.
    pub fn parse_response(
        body_text: &str,
        header_request_id: Option<&str>,
    ) -> Result<(String, Option<InferenceUsage>, Option<String>), AdapterError> {
        let v: serde_json::Value = serde_json::from_str(body_text).map_err(|e| {
            AdapterError::fatal(
                AdapterErrorKind::MalformedResponse,
                format!("invalid generateContent JSON: {e}"),
            )
        })?;
        if let Some(err) = v.get("error") {
            return Err(map_provider_error(err));
        }
        // generateContent has no body-level request id; prefer the header.
        let request_id = header_request_id
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let usage = v.get("usageMetadata").and_then(|u| {
            let input = u.get("promptTokenCount")?.as_u64()?;
            let output = u.get("candidatesTokenCount")?.as_u64()?;
            Some(InferenceUsage {
                input_tokens: input,
                output_tokens: output,
            })
        });
        let finish_reason = v
            .get("candidates")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finishReason"))
            .and_then(|f| f.as_str())
            .map(|s| s.to_string());
        Ok((request_id, usage, finish_reason))
    }
}

/// T035: typed quota errors. HTTP 429 (handled by the transport) and the
/// `RESOURCE_EXHAUSTED` error status both become retryable `Quota` errors.
fn map_provider_error(err: &serde_json::Value) -> AdapterError {
    let code = err.get("code").and_then(|c| c.as_u64()).unwrap_or(0);
    let status = err.get("status").and_then(|s| s.as_str()).unwrap_or("");
    let message = err
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("gemini error");
    // Bounded by the AdapterError constructors.
    let detail = format!("{status}/{code}: {message}");
    if code == 429 || status == "RESOURCE_EXHAUSTED" {
        AdapterError::retryable(AdapterErrorKind::Quota, detail)
    } else if code == 401 || code == 403 {
        AdapterError::fatal(AdapterErrorKind::Auth, detail)
    } else if code == 400 && message.contains("API key not valid") {
        AdapterError::fatal(AdapterErrorKind::Auth, detail)
    } else {
        AdapterError::fatal(AdapterErrorKind::Unknown, detail)
    }
}

#[async_trait]
impl InferenceAdapter for GeminiAdapter {
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
        // ModelId's charset ([A-Za-z0-9-_.:]) is URL-path-safe; no encoding needed.
        let url = format!(
            "{}/models/{}:generateContent",
            self.config.base_url(),
            self.config.model.as_str()
        );
        let api_key = match &req.credential {
            Some(r) => self.credentials.bearer_token(r)?,
            None => {
                return Err(AdapterError::fatal(
                    AdapterErrorKind::Auth,
                    "gemini requires a credential",
                ));
            }
        };
        let body = self.request_body(&req);
        let headers = [("x-goog-api-key", api_key.as_str())];
        let header_refs: Vec<(&str, &str)> =
            headers.iter().map(|(k, v)| (*k, *v)).collect();
        let deadline = Duration::from_millis(req.timeout_ms);
        let http_resp = tokio::time::timeout(
            deadline,
            self.transport
                .post_json(&url, None, &header_refs, &body, cancel),
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
            Ok("gemini-test-key".to_string())
        }
    }

    fn resolver() -> Arc<dyn CredentialResolver> {
        Arc::new(TestResolver)
    }

    fn adapter_for(url: &str) -> GeminiAdapter {
        let mut config = GeminiConfig::new(ModelId::new("gemini-2.0-flash").unwrap());
        config.base_url = Some(url.to_string());
        GeminiAdapter::new(EndpointId::new("ep-gemini").unwrap(), config, resolver()).unwrap()
    }

    fn sample_request() -> InferenceRequest {
        InferenceRequest {
            prompt: "say hi".to_string(),
            model: ModelId::new("gemini-2.0-flash").unwrap(),
            max_tokens: Some(64),
            structured_output: false,
            timeout_ms: 10_000,
            credential: Some(SecretRef::new("custom.gemini").unwrap()),
        }
    }

    #[test]
    fn request_body_shape() {
        let a = adapter_for("https://example.com");
        let mut req = sample_request();
        let body = a.request_body(&req);
        assert_eq!(body["contents"][0]["parts"][0]["text"], "say hi");
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 64);
        assert!(body["generationConfig"].get("responseMimeType").is_none());

        req.structured_output = true;
        req.max_tokens = None;
        let body = a.request_body(&req);
        assert_eq!(body["generationConfig"]["responseMimeType"], "application/json");
        assert!(body["generationConfig"].get("maxOutputTokens").is_none());
    }

    #[test]
    fn parse_response_extracts_usage_and_finish_reason() {
        let text = r#"{
            "candidates": [{
                "content": {"parts": [{"text": "hello"}], "role": "model"},
                "finishReason": "STOP"
            }],
            "usageMetadata": {"promptTokenCount": 7, "candidatesTokenCount": 3, "totalTokenCount": 10}
        }"#;
        let (id, usage, finish) = GeminiAdapter::parse_response(text, Some("hdr-9")).unwrap();
        assert_eq!(id, "hdr-9");
        assert_eq!(
            usage,
            Some(InferenceUsage {
                input_tokens: 7,
                output_tokens: 3
            })
        );
        assert_eq!(finish.as_deref(), Some("STOP"));
    }

    #[test]
    fn resource_exhausted_is_retryable_quota() {
        let text = r#"{"error": {"code": 429, "message": "quota exceeded", "status": "RESOURCE_EXHAUSTED"}}"#;
        let err = GeminiAdapter::parse_response(text, None).unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::Quota);
        assert!(err.is_retryable());
        // ... and the fabric mapping is QuotaExhausted (T035's typed quota error).
        assert!(matches!(
            err.to_fabric(),
            fabric_types::FabricError::QuotaExhausted(_)
        ));
    }

    #[test]
    fn invalid_api_key_is_fatal_auth() {
        let text = r#"{"error": {"code": 400, "message": "API key not valid. Please pass a valid API key.", "status": "INVALID_ARGUMENT"}}"#;
        let err = GeminiAdapter::parse_response(text, None).unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::Auth);
        assert!(!err.is_retryable());
    }

    #[test]
    fn malformed_json_is_malformed_response() {
        let err = GeminiAdapter::parse_response("nope", None).unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::MalformedResponse);
    }

    #[tokio::test]
    async fn infer_uses_generate_content_shape_and_key_header() {
        let body = br#"{
            "candidates": [{"content": {"parts": [{"text": "hi"}]}, "finishReason": "STOP"}],
            "usageMetadata": {"promptTokenCount": 4, "candidatesTokenCount": 2, "totalTokenCount": 6}
        }"#
        .to_vec();
        let (url, rx) = fake_server(FakeSpec::ok(body)).await;
        let a = adapter_for(&url);
        let resp = a.infer(sample_request(), &CancellationToken::new()).await.unwrap();
        assert_eq!(
            resp.usage,
            Some(InferenceUsage {
                input_tokens: 4,
                output_tokens: 2
            })
        );
        assert_eq!(resp.finish_reason.as_deref(), Some("STOP"));

        let raw = String::from_utf8(rx.recv().unwrap()).unwrap();
        assert!(raw.starts_with("POST /models/gemini-2.0-flash:generateContent "));
        assert!(raw.contains("x-goog-api-key: gemini-test-key"));
        assert!(!raw.contains("Authorization:"));
        assert!(raw.contains(r#""text":"say hi""#));
        assert!(raw.contains(r#""maxOutputTokens":64"#));
    }

    #[tokio::test]
    async fn infer_without_credential_is_fatal_auth() {
        let (url, _) = fake_server(FakeSpec::ok(br#"{}"#.to_vec())).await;
        let a = adapter_for(&url);
        let mut req = sample_request();
        req.credential = None;
        let err = a.infer(req, &CancellationToken::new()).await.unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::Auth);
        assert!(!err.is_retryable());
    }

    #[tokio::test]
    async fn http_429_maps_to_quota() {
        let (url, _) = fake_server(FakeSpec {
            status: 429,
            reason: "Too Many Requests",
            ..FakeSpec::ok(
                br#"{"error":{"code":429,"message":"quota","status":"RESOURCE_EXHAUSTED"}}"#.to_vec(),
            )
        })
        .await;
        let a = adapter_for(&url);
        let err = a
            .infer(sample_request(), &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::Quota);
        assert!(err.is_retryable());
    }
}
