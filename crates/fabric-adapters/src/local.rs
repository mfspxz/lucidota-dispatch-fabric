//! T037: local OpenAI-compatible inference adapter.
//!
//! Talks to a local OpenAI-compatible server (e.g. Ollama, llama.cpp server,
//! vLLM) over HTTP. Every request is bounded: connect timeout, total-response
//! timeout (the smaller of the request deadline and the configured cap), and a
//! hard response-size limit. [`LocalAdapter::health_probe`] checks liveness via
//! `GET {base}/models` before dispatch.
//!
//! Local servers are typically keyless; a [`SecretRef`](fabric_types::SecretRef)
//! in the request is accepted and ignored (never logged), so the adapter stays
//! provider-neutral.

use crate::errors::{AdapterError, AdapterErrorKind};
use crate::traits::{
    CancellationToken, InferenceAdapter, InferenceRequest, InferenceResponse, InferenceUsage,
};
use async_trait::async_trait;
use fabric_types::{EndpointId, ModelId};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// T037: configuration for the local adapter. All bounds explicit, no
/// hardcoded endpoints: the base URL is always supplied by the caller.
#[derive(Debug, Clone)]
pub struct LocalAdapterConfig {
    /// Base URL of the OpenAI-compatible server, e.g. `http://127.0.0.1:11434/v1`.
    pub base_url: String,
    pub endpoint_id: EndpointId,
    pub model: ModelId,
    /// TCP/TLS connect timeout.
    pub connect_timeout: Duration,
    /// Hard cap on total response time; the request's own `timeout_ms` is
    /// honored but never allowed to exceed this.
    pub max_response_time: Duration,
    /// Maximum accepted response body size in bytes.
    pub max_response_bytes: usize,
}

impl LocalAdapterConfig {
    pub fn new(
        base_url: impl Into<String>,
        endpoint_id: EndpointId,
        model: ModelId,
    ) -> Result<Self, AdapterError> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err(AdapterError::fatal(
                AdapterErrorKind::Transport,
                format!("base_url must start with http:// or https://, got `{base_url}`"),
            ));
        }
        // Credentials travel via SecretRef, never in the URL: reject embedded
        // userinfo so it cannot leak into error diagnostics.
        let authority = base_url
            .split("://")
            .nth(1)
            .unwrap_or("")
            .split('/')
            .next()
            .unwrap_or("");
        if authority.contains('@') {
            return Err(AdapterError::fatal(
                AdapterErrorKind::Transport,
                "base_url must not embed userinfo credentials; use a SecretRef",
            ));
        }
        Ok(Self {
            base_url,
            endpoint_id,
            model,
            connect_timeout: Duration::from_secs(5),
            max_response_time: Duration::from_secs(300),
            max_response_bytes: 4 * 1024 * 1024,
        })
    }

    pub fn connect_timeout(mut self, d: Duration) -> Self {
        self.connect_timeout = d;
        self
    }
    pub fn max_response_time(mut self, d: Duration) -> Self {
        self.max_response_time = d;
        self
    }
    pub fn max_response_bytes(mut self, n: usize) -> Self {
        self.max_response_bytes = n;
        self
    }
}

/// T037: local OpenAI-compatible adapter.
pub struct LocalAdapter {
    config: LocalAdapterConfig,
    client: reqwest::Client,
    endpoint_id: EndpointId,
}

impl LocalAdapter {
    pub fn new(config: LocalAdapterConfig) -> Result<Self, AdapterError> {
        let client = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            .build()
            .map_err(|e| {
                AdapterError::fatal(
                    AdapterErrorKind::Transport,
                    format!("failed to build HTTP client: {e}"),
                )
            })?;
        Ok(Self {
            endpoint_id: config.endpoint_id.clone(),
            config,
            client,
        })
    }

    /// Liveness probe: `GET {base_url}/models` within the connect timeout
    /// plus a small read budget. Any 2xx is healthy; anything else is a
    /// typed, non-retryable transport error (the server is reachable but the
    /// OpenAI-compatible surface is not there).
    pub async fn health_probe(&self) -> Result<(), AdapterError> {
        let url = format!("{}/models", self.config.base_url);
        let resp = tokio::time::timeout(
            self.config.connect_timeout + Duration::from_secs(5),
            self.client.get(&url).send(),
        )
        .await
        .map_err(|_| {
            AdapterError::retryable(AdapterErrorKind::Timeout, "health probe timed out")
        })?
        .map_err(|e| classify_reqwest_error(&e))?;
        let status = resp.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(AdapterError::fatal(
                AdapterErrorKind::Transport,
                format!("health probe: unexpected status {status} from {url}"),
            ))
        }
    }

    fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.config.base_url)
    }
}

#[derive(Debug, Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Serialize)]
struct ChatBody<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
}

#[derive(Debug, Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Debug, Deserialize)]
struct ChatReply {
    id: Option<String>,
    choices: Option<Vec<Choice>>,
    usage: Option<UsageBody>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UsageBody {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
}

#[async_trait]
impl InferenceAdapter for LocalAdapter {
    async fn infer(
        &self,
        req: InferenceRequest,
        cancel: &CancellationToken,
    ) -> Result<InferenceResponse, AdapterError> {
        if cancel.is_cancelled() {
            return Err(AdapterError::fatal(
                AdapterErrorKind::Cancelled,
                "inference cancelled before dispatch",
            ));
        }
        // The request deadline is honored but capped: bounded requests means
        // no caller can ask for an unbounded wait.
        let deadline = Duration::from_millis(req.timeout_ms.max(1)).min(self.config.max_response_time);

        let body = ChatBody {
            model: self.config.model.as_str(),
            messages: vec![ChatMessage {
                role: "user",
                content: &req.prompt,
            }],
            max_tokens: req.max_tokens,
            response_format: req.structured_output.then_some(ResponseFormat { kind: "json_object" }),
        };

        let send = self.client.post(self.chat_url()).json(&body).send();
        let resp = tokio::time::timeout(deadline, send)
            .await
            .map_err(|_| {
                AdapterError::retryable(
                    AdapterErrorKind::Timeout,
                    format!("local inference exceeded {}ms", deadline.as_millis()),
                )
            })?
            .map_err(|e| classify_reqwest_error(&e))?;

        if cancel.is_cancelled() {
            return Err(AdapterError::fatal(
                AdapterErrorKind::Cancelled,
                "inference cancelled while waiting for response",
            ));
        }

        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(AdapterError::retryable(
                AdapterErrorKind::Quota,
                "local server returned 429",
            ));
        }
        if status.is_server_error() {
            return Err(AdapterError::retryable(
                AdapterErrorKind::Transport,
                format!("local server error: {status}"),
            ));
        }
        if !status.is_success() {
            let kind = match status {
                reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                    AdapterErrorKind::Auth
                }
                _ => AdapterErrorKind::Transport,
            };
            return Err(AdapterError::fatal(
                kind,
                format!("local server returned {status}"),
            ));
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| classify_reqwest_error(&e))?;
        if bytes.len() > self.config.max_response_bytes {
            return Err(AdapterError::fatal(
                AdapterErrorKind::MalformedResponse,
                format!(
                    "response body {} bytes exceeds limit of {} bytes",
                    bytes.len(),
                    self.config.max_response_bytes
                ),
            ));
        }
        let raw_body = String::from_utf8(bytes.to_vec()).map_err(|_| {
            AdapterError::fatal(
                AdapterErrorKind::MalformedResponse,
                "response body is not valid UTF-8",
            )
        })?;

        let reply: ChatReply = serde_json::from_str(&raw_body).map_err(|e| {
            AdapterError::fatal(
                AdapterErrorKind::MalformedResponse,
                format!("response is not a valid chat-completion object: {e}"),
            )
        })?;
        let choice = reply.choices.and_then(|mut c| c.pop()).ok_or_else(|| {
            AdapterError::fatal(
                AdapterErrorKind::MalformedResponse,
                "chat-completion response has no choices",
            )
        })?;

        let request_id = reply
            .id
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("local-{}", std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)));
        let usage = reply.usage.map(|u| InferenceUsage {
            input_tokens: u.prompt_tokens.unwrap_or(0),
            output_tokens: u.completion_tokens.unwrap_or(0),
        });
        let finish_reason = choice.finish_reason;

        Ok(InferenceResponse {
            request_id,
            raw_body,
            usage,
            finish_reason,
        })
    }

    fn endpoint_id(&self) -> &EndpointId {
        &self.endpoint_id
    }
}

/// Map a reqwest transport failure onto the typed taxonomy. Timeouts are
/// retryable; connect/refused/reset are retryable transport errors; anything
/// else is a fatal transport error. No bodies or URLs with credentials are
/// included in the detail.
fn classify_reqwest_error(e: &reqwest::Error) -> AdapterError {
    if e.is_timeout() {
        return AdapterError::retryable(AdapterErrorKind::Timeout, "request timed out");
    }
    if e.is_connect() {
        return AdapterError::retryable(
            AdapterErrorKind::Transport,
            "could not connect to local server",
        );
    }
    AdapterError::fatal(AdapterErrorKind::Transport, format!("HTTP transport failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn test_config(port: u16) -> LocalAdapterConfig {
        LocalAdapterConfig::new(
            format!("http://127.0.0.1:{port}/v1"),
            EndpointId::new("local-test").unwrap(),
            ModelId::new("test-model").unwrap(),
        )
        .unwrap()
    }

    fn test_request() -> InferenceRequest {
        InferenceRequest {
            prompt: "hello".to_string(),
            model: ModelId::new("test-model").unwrap(),
            max_tokens: Some(16),
            structured_output: false,
            timeout_ms: 5000,
            credential: None,
        }
    }

    /// Minimal canned HTTP server on 127.0.0.1. `handler` receives the raw
    /// request bytes and returns (status_line, body). Serves exactly one
    /// connection per spawned task; tests spawn one task per request.
    async fn canned_server(handler: impl Fn(Vec<u8>) -> (String, String) + Send + Sync + 'static) -> u16 {
        use std::sync::Arc;
        let handler = Arc::new(handler);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 65536];
                // Read until end of headers; body (if any) is ignored.
                let mut req = Vec::new();
                loop {
                    match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            req.extend_from_slice(&buf[..n]);
                            if req.windows(4).any(|w| w == b"\r\n\r\n") || req.len() > 60000 {
                                break;
                            }
                        }
                    }
                }
                let (status, body) = handler(req);
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        // Give the accept loop a moment to start.
        tokio::time::sleep(Duration::from_millis(25)).await;
        port
    }

    const CHAT_OK: &str = r#"{"id":"chatcmpl-1","choices":[{"message":{"content":"hi there"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":2}}"#;

    #[test]
    fn config_rejects_non_http_base_url() {
        let r = LocalAdapterConfig::new(
            "ftp://example.com",
            EndpointId::new("e1").unwrap(),
            ModelId::new("m1").unwrap(),
        );
        assert!(r.is_err());
    }

    #[test]
    fn config_rejects_embedded_userinfo() {
        let r = LocalAdapterConfig::new(
            "http://user:pass@127.0.0.1:11434/v1",
            EndpointId::new("e1").unwrap(),
            ModelId::new("m1").unwrap(),
        );
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn health_probe_ok() {
        let port = canned_server(|_| ("200 OK".to_string(), r#"{"data":[]}"#.to_string())).await;
        let a = LocalAdapter::new(test_config(port)).unwrap();
        assert!(a.health_probe().await.is_ok());
    }

    #[tokio::test]
    async fn health_probe_non_2xx_is_fatal() {
        let port = canned_server(|_| ("404 Not Found".to_string(), "{}".to_string())).await;
        let a = LocalAdapter::new(test_config(port)).unwrap();
        let e = a.health_probe().await.unwrap_err();
        assert_eq!(e.kind, AdapterErrorKind::Transport);
        assert!(!e.is_retryable());
    }

    #[tokio::test]
    async fn health_probe_refused_is_retryable_transport() {
        // Nothing listens here: connect refused.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let a = LocalAdapter::new(test_config(port)).unwrap();
        let e = a.health_probe().await.unwrap_err();
        assert!(e.is_retryable());
    }

    #[tokio::test]
    async fn infer_ok_parses_chat_completion() {
        let port = canned_server(|_| ("200 OK".to_string(), CHAT_OK.to_string())).await;
        let a = LocalAdapter::new(test_config(port)).unwrap();
        let resp = a.infer(test_request(), &CancellationToken::new()).await.unwrap();
        assert_eq!(resp.request_id, "chatcmpl-1");
        assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
        let usage = resp.usage.unwrap();
        assert_eq!(usage.input_tokens, 5);
        assert_eq!(usage.output_tokens, 2);
        assert!(resp.raw_body.contains("hi there"));
        assert_eq!(a.endpoint_id().as_str(), "local-test");
    }

    #[tokio::test]
    async fn infer_sends_openai_compatible_body() {
        let port = canned_server(|req| {
            let text = String::from_utf8_lossy(&req).into_owned();
            assert!(text.contains("\"model\":\"test-model\""), "model missing: {text}");
            assert!(text.contains("\"role\":\"user\""), "role missing: {text}");
            assert!(text.contains("hello"), "prompt missing: {text}");
            ("200 OK".to_string(), CHAT_OK.to_string())
        })
        .await;
        let a = LocalAdapter::new(test_config(port)).unwrap();
        assert!(a.infer(test_request(), &CancellationToken::new()).await.is_ok());
    }

    #[tokio::test]
    async fn infer_structured_output_requests_json_object() {
        let port = canned_server(|req| {
            let text = String::from_utf8_lossy(&req).into_owned();
            assert!(text.contains("json_object"), "response_format missing: {text}");
            ("200 OK".to_string(), CHAT_OK.to_string())
        })
        .await;
        let a = LocalAdapter::new(test_config(port)).unwrap();
        let mut req = test_request();
        req.structured_output = true;
        assert!(a.infer(req, &CancellationToken::new()).await.is_ok());
    }

    #[tokio::test]
    async fn infer_timeout_is_retryable() {
        let port = canned_server(|_| {
            std::thread::sleep(Duration::from_millis(800));
            ("200 OK".to_string(), CHAT_OK.to_string())
        })
        .await;
        let mut cfg = test_config(port);
        cfg = cfg.max_response_time(Duration::from_millis(150));
        let a = LocalAdapter::new(cfg).unwrap();
        let mut req = test_request();
        req.timeout_ms = 100;
        let e = a.infer(req, &CancellationToken::new()).await.unwrap_err();
        assert_eq!(e.kind, AdapterErrorKind::Timeout);
        assert!(e.is_retryable());
    }

    #[tokio::test]
    async fn infer_oversize_body_rejected() {
        let big = "x".repeat(2048);
        let body = format!(
            r#"{{"id":"c","choices":[{{"message":{{"content":"{big}"}},"finish_reason":"length"}}]}}"#
        );
        let port = canned_server(move |_| ("200 OK".to_string(), body.clone())).await;
        let mut cfg = test_config(port);
        cfg = cfg.max_response_bytes(64);
        let a = LocalAdapter::new(cfg).unwrap();
        let e = a.infer(test_request(), &CancellationToken::new()).await.unwrap_err();
        assert_eq!(e.kind, AdapterErrorKind::MalformedResponse);
        assert!(!e.is_retryable());
    }

    #[tokio::test]
    async fn infer_malformed_json_is_fatal() {
        let port = canned_server(|_| ("200 OK".to_string(), "not json{".to_string())).await;
        let a = LocalAdapter::new(test_config(port)).unwrap();
        let e = a.infer(test_request(), &CancellationToken::new()).await.unwrap_err();
        assert_eq!(e.kind, AdapterErrorKind::MalformedResponse);
        assert!(!e.is_retryable());
    }

    #[tokio::test]
    async fn infer_empty_choices_is_fatal() {
        let port = canned_server(|_| ("200 OK".to_string(), r#"{"id":"c","choices":[]}"#.to_string())).await;
        let a = LocalAdapter::new(test_config(port)).unwrap();
        let e = a.infer(test_request(), &CancellationToken::new()).await.unwrap_err();
        assert_eq!(e.kind, AdapterErrorKind::MalformedResponse);
    }

    #[tokio::test]
    async fn infer_500_is_retryable_transport() {
        let port = canned_server(|_| ("500 Internal Server Error".to_string(), "{}".to_string())).await;
        let a = LocalAdapter::new(test_config(port)).unwrap();
        let e = a.infer(test_request(), &CancellationToken::new()).await.unwrap_err();
        assert_eq!(e.kind, AdapterErrorKind::Transport);
        assert!(e.is_retryable());
    }

    #[tokio::test]
    async fn infer_429_is_retryable_quota() {
        let port = canned_server(|_| ("429 Too Many Requests".to_string(), "{}".to_string())).await;
        let a = LocalAdapter::new(test_config(port)).unwrap();
        let e = a.infer(test_request(), &CancellationToken::new()).await.unwrap_err();
        assert_eq!(e.kind, AdapterErrorKind::Quota);
        assert!(e.is_retryable());
    }

    #[tokio::test]
    async fn cancelled_before_dispatch() {
        let port = canned_server(|_| ("200 OK".to_string(), CHAT_OK.to_string())).await;
        let a = LocalAdapter::new(test_config(port)).unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let e = a.infer(test_request(), &cancel).await.unwrap_err();
        assert_eq!(e.kind, AdapterErrorKind::Cancelled);
        assert!(!e.is_retryable());
    }
}
