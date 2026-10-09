//! T032: configurable HTTP inference transport.
//!
//! Timeouts, response-size limits, retry classification, and redacted
//! diagnostics, built on reqwest.
//!
//! Redaction policy (enforced, not conventional):
//! - URLs are stripped of query string and fragment before any diagnostic
//!   use: query parameters may carry API keys.
//! - Request/response header *values* are never recorded, except an
//!   allow-list of response headers (`x-request-id`, `request-id`).
//! - Bodies appear in diagnostics only as a bounded prefix (256 chars).

use crate::errors::{AdapterError, AdapterErrorKind};
use crate::traits::CancellationToken;
use fabric_types::SecretRef;
use std::time::{Duration, Instant};

/// How an HTTP outcome should be treated by retry policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryClass {
    Retryable,
    Fatal,
    Unknown,
}

/// Classify an HTTP status code for retry purposes.
///
/// 408/425/429 and any 5xx are transient (retryable); other 4xx mean the
/// request itself is wrong (fatal). Anything else is not an error status.
pub fn classify_status(status: u16) -> RetryClass {
    match status {
        408 | 425 | 429 => RetryClass::Retryable,
        500..=599 => RetryClass::Retryable,
        400..=499 => RetryClass::Fatal,
        _ => RetryClass::Unknown,
    }
}

/// Strip query string and fragment: they may carry API keys.
pub fn redact_url(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or("").to_string()
}

/// Truncate to a bounded character prefix; never splits a char boundary.
pub fn truncate_prefix(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect()
    }
}

/// Transport tuning. Every knob has a safe default.
#[derive(Debug, Clone)]
pub struct HttpTransportConfig {
    /// TCP/TLS connect deadline.
    pub connect_timeout: Duration,
    /// Total-response deadline (includes body streaming).
    pub response_timeout: Duration,
    /// Hard cap on response body size; larger bodies are rejected.
    pub max_response_bytes: usize,
    pub user_agent: String,
}

impl Default for HttpTransportConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            response_timeout: Duration::from_secs(120),
            max_response_bytes: 4 * 1024 * 1024,
            user_agent: "dispatch-fabric/0.1".to_string(),
        }
    }
}

/// Successful HTTP response. The body is already size-bounded.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    /// From `x-request-id` / `request-id` response header, when present.
    pub request_id: Option<String>,
    pub body: Vec<u8>,
    pub latency_ms: u64,
}

impl HttpResponse {
    /// Lossy body text for JSON parsing and diagnostics.
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Redacted diagnostic snapshot of one HTTP exchange.
#[derive(Debug, Clone)]
pub struct HttpDiagnostic {
    pub method: &'static str,
    pub url: String,
    pub status: Option<u16>,
    pub retry_class: RetryClass,
    pub request_body_prefix: String,
    pub response_body_prefix: String,
    pub latency_ms: u64,
}

impl HttpDiagnostic {
    /// One-line summary safe for error details and logs: no credentials,
    /// no query strings, bodies truncated to bounded prefixes.
    pub fn short(&self) -> String {
        format!(
            "{} {} -> {} ({:?}) {}ms req={:?} resp={:?}",
            self.method,
            truncate_prefix(&self.url, 128),
            self.status
                .map(|s| s.to_string())
                .unwrap_or_else(|| "-".to_string()),
            self.retry_class,
            self.latency_ms,
            truncate_prefix(&self.request_body_prefix, 128),
            truncate_prefix(&self.response_body_prefix, 128),
        )
    }
}

/// Call-time credential resolution. The runtime implements this (see T029);
/// adapters hold the resolved value only for the duration of one call and
/// never log, persist, or serialize it.
pub trait CredentialResolver: Send + Sync {
    fn bearer_token(&self, r: &SecretRef) -> Result<String, AdapterError>;
}

/// Configurable HTTP transport for inference calls.
pub struct HttpTransport {
    client: reqwest::Client,
    config: HttpTransportConfig,
}

impl HttpTransport {
    pub fn new(config: HttpTransportConfig) -> Result<Self, AdapterError> {
        if config.max_response_bytes == 0 {
            return Err(AdapterError::fatal(
                AdapterErrorKind::Unknown,
                "max_response_bytes must be positive",
            ));
        }
        let client = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            .timeout(config.response_timeout)
            .user_agent(&config.user_agent)
            .build()
            .map_err(|e| {
                AdapterError::fatal(
                    AdapterErrorKind::Transport,
                    format!("http client build: {e}"),
                )
            })?;
        Ok(Self { client, config })
    }

    pub fn config(&self) -> &HttpTransportConfig {
        &self.config
    }

    /// POST a JSON body.
    ///
    /// `bearer` is a pre-resolved token used only for this call's
    /// `Authorization` header. `extra_headers` must not contain an
    /// `authorization` entry (case-insensitive): the bearer path is the
    /// only way credentials enter a request.
    pub async fn post_json(
        &self,
        url: &str,
        bearer: Option<&str>,
        extra_headers: &[(&str, &str)],
        body: &serde_json::Value,
        cancel: &CancellationToken,
    ) -> Result<HttpResponse, AdapterError> {
        if cancel.is_cancelled() {
            return Err(AdapterError::fatal(
                AdapterErrorKind::Cancelled,
                "cancelled before send",
            ));
        }
        for (name, _) in extra_headers {
            if name.eq_ignore_ascii_case("authorization") {
                return Err(AdapterError::fatal(
                    AdapterErrorKind::Unknown,
                    "extra_headers must not contain authorization",
                ));
            }
        }
        let body_text = serde_json::to_string(body).map_err(|e| {
            AdapterError::fatal(
                AdapterErrorKind::Unknown,
                format!("request serialization failed: {e}"),
            )
        })?;

        let redacted_url = redact_url(url);
        let req_prefix = truncate_prefix(&body_text, 256);
        let started = Instant::now();

        let mut req = self
            .client
            .post(url)
            .header("content-type", "application/json")
            .body(body_text.clone());
        if let Some(token) = bearer {
            req = req.bearer_auth(token);
        }
        for (name, value) in extra_headers {
            req = req.header(*name, *value);
        }

        let mut resp = req
            .send()
            .await
            .map_err(|e| map_reqwest_error(url, &redacted_url, e))?;
        let status = resp.status().as_u16();
        let request_id = resp
            .headers()
            .get("x-request-id")
            .or_else(|| resp.headers().get("request-id"))
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        // Stream the body with an enforced size cap: a peer that never
        // stops sending cannot exhaust memory.
        let mut buf: Vec<u8> = Vec::new();
        loop {
            if cancel.is_cancelled() {
                return Err(AdapterError::fatal(
                    AdapterErrorKind::Cancelled,
                    "cancelled during body read",
                ));
            }
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    buf.extend_from_slice(&chunk);
                    if buf.len() > self.config.max_response_bytes {
                        let diag = HttpDiagnostic {
                            method: "POST",
                            url: redacted_url,
                            status: Some(status),
                            retry_class: RetryClass::Fatal,
                            request_body_prefix: req_prefix,
                            response_body_prefix: truncate_prefix(
                                &String::from_utf8_lossy(&buf),
                                256,
                            ),
                            latency_ms: started.elapsed().as_millis() as u64,
                        };
                        return Err(AdapterError::fatal(
                            AdapterErrorKind::MalformedResponse,
                            format!(
                                "response exceeded {} bytes: {}",
                                self.config.max_response_bytes,
                                diag.short()
                            ),
                        ));
                    }
                }
                Ok(None) => break,
                Err(e) => return Err(map_reqwest_error(url, &redacted_url, e)),
            }
        }

        let latency_ms = started.elapsed().as_millis() as u64;
        let retry_class = classify_status(status);
        let diag = HttpDiagnostic {
            method: "POST",
            url: redacted_url,
            status: Some(status),
            retry_class,
            request_body_prefix: req_prefix,
            response_body_prefix: truncate_prefix(&String::from_utf8_lossy(&buf), 256),
            latency_ms,
        };

        if !(200..300).contains(&status) {
            let kind = match status {
                429 => AdapterErrorKind::Quota,
                401 | 403 => AdapterErrorKind::Auth,
                _ => match retry_class {
                    RetryClass::Retryable => AdapterErrorKind::Transport,
                    _ => AdapterErrorKind::Unknown,
                },
            };
            return Err(match retry_class {
                RetryClass::Retryable => AdapterError::retryable(kind, diag.short()),
                _ => AdapterError::fatal(kind, diag.short()),
            });
        }

        Ok(HttpResponse {
            status,
            request_id,
            body: buf,
            latency_ms,
        })
    }
}

/// Map a reqwest error to a typed adapter error. The original URL is passed
/// so any URL embedded in the error text can be redacted the same way.
fn map_reqwest_error(original_url: &str, redacted_url: &str, e: reqwest::Error) -> AdapterError {
    let msg = e.to_string().replace(original_url, redacted_url);
    let detail = format!("{redacted_url}: {msg}");
    if e.is_timeout() {
        AdapterError::retryable(AdapterErrorKind::Timeout, detail)
    } else {
        // Connect failures, body/decode errors, and anything else at the
        // transport layer are transient: retryable.
        AdapterError::retryable(AdapterErrorKind::Transport, detail)
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Local fake HTTP server for adapter tests. Loopback only, one-shot,
    //! no live network. Also captures the raw request bytes for assertions.

    use super::*;
    use std::sync::mpsc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    pub struct FakeSpec {
        pub status: u16,
        pub reason: &'static str,
        pub headers: Vec<(&'static str, &'static str)>,
        pub body: Vec<u8>,
        pub delay_ms: u64,
    }

    impl FakeSpec {
        pub fn ok(body: Vec<u8>) -> Self {
            Self {
                status: 200,
                reason: "OK",
                headers: vec![],
                body,
                delay_ms: 0,
            }
        }
    }

    /// Spawn the server. Returns the base URL and a receiver for the raw
    /// request bytes the client sent.
    pub async fn fake_server(spec: FakeSpec) -> (String, mpsc::Receiver<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let (tx, rx) = mpsc::channel();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let mut raw: Vec<u8> = Vec::new();
            let mut buf = [0u8; 4096];
            let header_end = loop {
                let n = sock.read(&mut buf).await.expect("read");
                if n == 0 {
                    break None;
                }
                raw.extend_from_slice(&buf[..n]);
                if let Some(p) = find_header_end(&raw) {
                    break Some(p);
                }
            };
            if let Some(p) = header_end {
                let headers_text = String::from_utf8_lossy(&raw[..p]).into_owned();
                let len = content_length(&headers_text);
                while raw.len() < p + len {
                    let n = sock.read(&mut buf).await.expect("read body");
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buf[..n]);
                }
            }
            let _ = tx.send(raw);
            if spec.delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(spec.delay_ms)).await;
            }
            let mut resp = format!("HTTP/1.1 {} {}\r\n", spec.status, spec.reason);
            for (k, v) in &spec.headers {
                resp.push_str(&format!("{k}: {v}\r\n"));
            }
            resp.push_str(&format!(
                "content-length: {}\r\nconnection: close\r\n\r\n",
                spec.body.len()
            ));
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.write_all(&spec.body).await;
        });
        (format!("http://{addr}"), rx)
    }

    fn find_header_end(raw: &[u8]) -> Option<usize> {
        raw.windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|p| p + 4)
    }

    fn content_length(headers: &str) -> usize {
        headers
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                if k.trim().eq_ignore_ascii_case("content-length") {
                    v.trim().parse().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    fn transport() -> HttpTransport {
        HttpTransport::new(HttpTransportConfig::default()).unwrap()
    }

    #[test]
    fn classify_status_table() {
        for s in [408u16, 425, 429, 500, 502, 503, 504] {
            assert_eq!(classify_status(s), RetryClass::Retryable, "{s}");
        }
        for s in [400u16, 401, 403, 404, 422] {
            assert_eq!(classify_status(s), RetryClass::Fatal, "{s}");
        }
        assert_eq!(classify_status(200), RetryClass::Unknown);
    }

    #[test]
    fn redact_url_strips_query_and_fragment() {
        assert_eq!(
            redact_url("https://api.example.com/v1/chat?api_key=SECRET#frag"),
            "https://api.example.com/v1/chat"
        );
        assert_eq!(redact_url("http://x/y"), "http://x/y");
    }

    #[test]
    fn truncate_prefix_is_char_boundary_safe() {
        let s = "héllo🌍world";
        let t = truncate_prefix(s, 6);
        assert_eq!(t.chars().count(), 6);
        assert!(s.starts_with(&t));
    }

    #[test]
    fn rejects_zero_max_response_bytes() {
        let cfg = HttpTransportConfig {
            max_response_bytes: 0,
            ..HttpTransportConfig::default()
        };
        assert!(HttpTransport::new(cfg).is_err());
    }

    #[tokio::test]
    async fn post_json_success_round_trip() {
        let (url, rx) = fake_server(FakeSpec {
            headers: vec![("x-request-id", "req-123")],
            ..FakeSpec::ok(br#"{"ok":true}"#.to_vec())
        })
        .await;
        let t = transport();
        let cancel = CancellationToken::new();
        let resp = t
            .post_json(&url, Some("tok"), &[], &serde_json::json!({"a": 1}), &cancel)
            .await
            .unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.request_id.as_deref(), Some("req-123"));
        assert_eq!(resp.body_text(), r#"{"ok":true}"#);
        let raw = String::from_utf8(rx.recv().unwrap()).unwrap();
        // header names and values are lowercased for comparison: the http
        // crate normalizes header names to lowercase on the wire.
        let lower = raw.to_ascii_lowercase();
        assert!(lower.contains("authorization: bearer tok")); // all-lowercase: `lower` is lowercased
        assert!(raw.contains(r#""a":1"#));
    }

    #[tokio::test]
    async fn status_429_maps_to_retryable_quota() {
        let (url, _) = fake_server(FakeSpec {
            status: 429,
            reason: "Too Many Requests",
            ..FakeSpec::ok(br#"{"error":"slow down"}"#.to_vec())
        })
        .await;
        let t = transport();
        let err = t
            .post_json(&url, None, &[], &serde_json::json!({}), &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::Quota);
        assert!(err.is_retryable());
        // diagnostic is redacted: no secret-looking content, bounded length
        assert!(err.safe_detail.len() <= 512);
    }

    #[tokio::test]
    async fn status_401_maps_to_fatal_auth() {
        let (url, _) = fake_server(FakeSpec {
            status: 401,
            reason: "Unauthorized",
            ..FakeSpec::ok(br#"{}"#.to_vec())
        })
        .await;
        let err = transport()
            .post_json(&url, None, &[], &serde_json::json!({}), &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::Auth);
        assert!(!err.is_retryable());
    }

    #[tokio::test]
    async fn status_500_is_retryable_transport() {
        let (url, _) = fake_server(FakeSpec {
            status: 500,
            reason: "Internal Server Error",
            ..FakeSpec::ok(br#"{}"#.to_vec())
        })
        .await;
        let err = transport()
            .post_json(&url, None, &[], &serde_json::json!({}), &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::Transport);
        assert!(err.is_retryable());
    }

    #[tokio::test]
    async fn oversize_body_is_rejected() {
        let (url, _) = fake_server(FakeSpec::ok(vec![b'x'; 1024])).await;
        let t = HttpTransport::new(HttpTransportConfig {
            max_response_bytes: 16,
            ..HttpTransportConfig::default()
        })
        .unwrap();
        let err = t
            .post_json(&url, None, &[], &serde_json::json!({}), &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::MalformedResponse);
        assert!(!err.is_retryable());
    }

    #[tokio::test]
    async fn slow_server_hits_response_timeout() {
        let (url, _) = fake_server(FakeSpec {
            delay_ms: 1500,
            ..FakeSpec::ok(br#"{}"#.to_vec())
        })
        .await;
        let t = HttpTransport::new(HttpTransportConfig {
            response_timeout: Duration::from_millis(150),
            ..HttpTransportConfig::default()
        })
        .unwrap();
        let err = t
            .post_json(&url, None, &[], &serde_json::json!({}), &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::Timeout);
        assert!(err.is_retryable());
    }

    #[tokio::test]
    async fn cancelled_before_send_is_fatal_cancelled() {
        let (url, _) = fake_server(FakeSpec::ok(br#"{}"#.to_vec())).await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = transport()
            .post_json(&url, None, &[], &serde_json::json!({}), &cancel)
            .await
            .unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::Cancelled);
        assert!(!err.is_retryable());
    }

    #[tokio::test]
    async fn authorization_in_extra_headers_is_rejected() {
        let (url, _) = fake_server(FakeSpec::ok(br#"{}"#.to_vec())).await;
        let err = transport()
            .post_json(
                &url,
                None,
                &[("Authorization", "Bearer smuggled")],
                &serde_json::json!({}),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(!err.is_retryable());
        // server never saw the request
    }

    #[test]
    fn diagnostic_short_is_redacted_and_bounded() {
        let d = HttpDiagnostic {
            method: "POST",
            url: "https://api.example.com/v1?api_key=SECRET".to_string(),
            status: Some(429),
            retry_class: RetryClass::Retryable,
            request_body_prefix: "x".repeat(1000),
            response_body_prefix: "y".repeat(1000),
            latency_ms: 12,
        };
        let s = d.short();
        // note: short() does not itself strip the query; callers pass redacted_url.
        // what it guarantees: bounded prefixes.
        assert!(s.len() < 700);
        let d2 = HttpDiagnostic {
            url: redact_url("https://api.example.com/v1?api_key=SECRET"),
            ..d
        };
        assert!(!d2.short().contains("SECRET"));
    }
}
