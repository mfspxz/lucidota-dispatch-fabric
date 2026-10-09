//! T034: NVIDIA inference adapter.
//!
//! NVIDIA NIM exposes an OpenAI-compatible API, so this adapter reuses the
//! OpenAI-compatible implementation and adds NVIDIA's documented endpoint
//! settings.
//!
//! Quota rule (hard): quotas are never hardcoded. They arrive only via the
//! `Option<QuotaHints>` constructor parameter, which the quota engine
//! (T041+) consumes. `None` means the limit is unknown, and unknown stays
//! unknown.

use crate::errors::AdapterError;
use crate::http::CredentialResolver;
use crate::openai::{OpenAiAdapter, OpenAiConfig};
use crate::traits::{CancellationToken, InferenceAdapter, InferenceRequest, InferenceResponse};
use async_trait::async_trait;
use fabric_types::{EndpointId, ModelId};
use std::sync::Arc;

/// Documented NVIDIA NIM base URL.
pub const NVIDIA_DEFAULT_BASE_URL: &str = "https://integrate.api.nvidia.com/v1";

/// Documented endpoint settings for NVIDIA NIM.
#[derive(Debug, Clone)]
pub struct NvidiaSettings {
    /// Override the base URL. Defaults to [`NVIDIA_DEFAULT_BASE_URL`]
    /// when `None`.
    pub base_url: Option<String>,
    pub model: ModelId,
}

/// Quota hints supplied by configuration — the ONLY source of quota
/// knowledge in this adapter. Never hardcoded; `None` fields mean the
/// corresponding limit is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QuotaHints {
    pub requests_per_minute: Option<u32>,
    pub tokens_per_minute: Option<u32>,
}

/// NVIDIA NIM adapter (OpenAI-compatible wire shape).
pub struct NvidiaAdapter {
    endpoint_id: EndpointId,
    inner: OpenAiAdapter,
    quota_hints: Option<QuotaHints>,
}

impl NvidiaAdapter {
    pub fn new(
        endpoint_id: EndpointId,
        settings: NvidiaSettings,
        quota_hints: Option<QuotaHints>,
        credentials: Arc<dyn CredentialResolver>,
    ) -> Result<Self, AdapterError> {
        let base = settings
            .base_url
            .unwrap_or_else(|| NVIDIA_DEFAULT_BASE_URL.to_string());
        let config = OpenAiConfig::new(base, settings.model)?;
        let inner = OpenAiAdapter::new(endpoint_id.clone(), config, credentials)?;
        Ok(Self {
            endpoint_id,
            inner,
            quota_hints,
        })
    }

    /// Quota hints for the quota engine. `None` (or `None` fields) means
    /// unknown limits — never inferred, never defaulted.
    pub fn quota_hints(&self) -> Option<QuotaHints> {
        self.quota_hints
    }
}

#[async_trait]
impl InferenceAdapter for NvidiaAdapter {
    async fn infer(
        &self,
        req: InferenceRequest,
        cancel: &CancellationToken,
    ) -> Result<InferenceResponse, AdapterError> {
        self.inner.infer(req, cancel).await
    }

    fn endpoint_id(&self) -> &EndpointId {
        &self.endpoint_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::AdapterErrorKind;
    use crate::http::test_support::{FakeSpec, fake_server};
    use crate::traits::InferenceUsage;
    use fabric_types::SecretRef;

    struct TestResolver;
    impl CredentialResolver for TestResolver {
        fn bearer_token(&self, _r: &SecretRef) -> Result<String, AdapterError> {
            Ok("nv-test-token".to_string())
        }
    }

    fn resolver() -> Arc<dyn CredentialResolver> {
        Arc::new(TestResolver)
    }

    fn settings(base: Option<&str>) -> NvidiaSettings {
        NvidiaSettings {
            base_url: base.map(|s| s.to_string()),
            model: ModelId::new("meta.llama-test").unwrap(),
        }
    }

    #[test]
    fn default_base_url_is_documented_endpoint() {
        let a = NvidiaAdapter::new(
            EndpointId::new("ep-nv").unwrap(),
            settings(None),
            None,
            resolver(),
        )
        .unwrap();
        assert_eq!(
            a.inner.config().base_url,
            "https://integrate.api.nvidia.com/v1"
        );
        // No quotas are known unless explicitly supplied.
        assert_eq!(a.quota_hints(), None);
    }

    #[test]
    fn custom_base_url_and_hints_pass_through() {
        let hints = QuotaHints {
            requests_per_minute: Some(40),
            tokens_per_minute: None,
        };
        let a = NvidiaAdapter::new(
            EndpointId::new("ep-nv").unwrap(),
            settings(Some("https://nim.local/v1/")),
            Some(hints),
            resolver(),
        )
        .unwrap();
        assert_eq!(a.inner.config().base_url, "https://nim.local/v1");
        assert_eq!(a.quota_hints(), Some(hints));
        // Unknown stays unknown: the unset field is None, not a guess.
        assert_eq!(a.quota_hints().unwrap().tokens_per_minute, None);
    }

    #[tokio::test]
    async fn infer_uses_openai_wire_shape_and_bearer() {
        let body = br#"{
            "id": "nv-1",
            "choices": [{"finish_reason": "stop", "message": {"content": "ok"}}],
            "usage": {"prompt_tokens": 8, "completion_tokens": 4, "total_tokens": 12}
        }"#
        .to_vec();
        let (url, rx) = fake_server(FakeSpec::ok(body)).await;
        let a = NvidiaAdapter::new(
            EndpointId::new("ep-nv").unwrap(),
            settings(Some(&url)),
            None,
            resolver(),
        )
        .unwrap();
        let req = InferenceRequest {
            prompt: "ping".to_string(),
            model: ModelId::new("meta.llama-test").unwrap(),
            max_tokens: None,
            structured_output: false,
            timeout_ms: 10_000,
            credential: Some(SecretRef::new("custom.nvidia-nim").unwrap()),
        };
        let resp = a.infer(req, &CancellationToken::new()).await.unwrap();
        assert_eq!(resp.request_id, "nv-1");
        assert_eq!(
            resp.usage,
            Some(InferenceUsage {
                input_tokens: 8,
                output_tokens: 4
            })
        );
        let raw = String::from_utf8(rx.recv().unwrap()).unwrap();
        assert!(raw.starts_with("POST /chat/completions "));
        assert!(raw.to_ascii_lowercase().contains("authorization: bearer nv-test-token"));
    }

    #[tokio::test]
    async fn nim_429_maps_to_quota() {
        let (url, _) = fake_server(FakeSpec {
            status: 429,
            reason: "Too Many Requests",
            ..FakeSpec::ok(br#"{"detail":"rate limited"}"#.to_vec())
        })
        .await;
        let a = NvidiaAdapter::new(
            EndpointId::new("ep-nv").unwrap(),
            settings(Some(&url)),
            None,
            resolver(),
        )
        .unwrap();
        let req = InferenceRequest {
            prompt: "ping".to_string(),
            model: ModelId::new("meta.llama-test").unwrap(),
            max_tokens: None,
            structured_output: false,
            timeout_ms: 10_000,
            credential: None,
        };
        let err = a.infer(req, &CancellationToken::new()).await.unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::Quota);
        assert!(err.is_retryable());
    }
}
