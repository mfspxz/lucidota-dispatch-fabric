//! T040: adapter conformance harness.
//!
//! Reusable conformance gate for any [`InferenceAdapter`]. Runs five checks —
//! timeout, malformed response, cancellation, usage accounting, and secret
//! redaction — against the adapter under test and returns a structured
//! [`ConformanceReport`].
//!
//! Checks that need provider misbehavior (slow / malformed / usage-bearing /
//! canary-bearing responses) are steered through a [`ProviderRig`]: a
//! loopback-only fake HTTP provider owned by the harness. No live network is
//! ever touched; the rig binds 127.0.0.1 on an ephemeral port.
//!
//! Outcome discipline is trinary ([`Outcome`]): Pass, Fail, Inconclusive. A
//! check that cannot be evidenced is Inconclusive, never Pass.

use crate::errors::AdapterErrorKind;
use crate::traits::{CancellationToken, InferenceAdapter, InferenceRequest, InferenceUsage};
use fabric_types::{ModelId, SecretRef};
use serde::Serialize;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// T040: trinary check outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Outcome {
    Pass,
    Fail,
    Inconclusive,
}

/// T040: one check's result plus the evidence behind it.
#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub check: &'static str,
    pub outcome: Outcome,
    pub evidence: String,
}

/// T040: structured conformance report for one adapter.
#[derive(Debug, Clone, Serialize)]
pub struct ConformanceReport {
    pub adapter: String,
    pub results: Vec<CheckResult>,
}

impl ConformanceReport {
    pub fn passed(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.outcome == Outcome::Pass)
            .count()
    }

    pub fn failed(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.outcome == Outcome::Fail)
            .count()
    }

    pub fn inconclusive(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.outcome == Outcome::Inconclusive)
            .count()
    }

    /// Green means zero failures. Inconclusive checks are reported, never hidden.
    pub fn is_green(&self) -> bool {
        !self.results.is_empty() && self.failed() == 0
    }
}

/// T040: harness tuning. Every bound is explicit; there is no hidden magic.
#[derive(Debug, Clone)]
pub struct HarnessConfig {
    /// Request deadline used by the timeout check.
    pub timeout_check_ms: u64,
    /// Scheduling slack added to measured-time assertions.
    pub timeout_slack_ms: u64,
    /// How long the fake provider stalls for the timeout check.
    pub slow_provider_delay_ms: u64,
    /// Deadline for checks that expect a prompt answer.
    pub request_timeout_ms: u64,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            timeout_check_ms: 300,
            timeout_slack_ms: 1500,
            slow_provider_delay_ms: 3000,
            request_timeout_ms: 10_000,
        }
    }
}

/// Wire shape the fake provider must speak for the adapter under test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireShape {
    OpenAiChatCompletions,
    GeminiGenerateContent,
}

impl WireShape {
    fn ok_body(&self) -> Vec<u8> {
        match self {
            Self::OpenAiChatCompletions => br#"{"id":"chatcmpl-harness","choices":[{"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30}}"#.to_vec(),
            Self::GeminiGenerateContent => br#"{"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":20,"totalTokenCount":30}}"#.to_vec(),
        }
    }
}

/// Scripted fake-provider behavior.
#[derive(Debug, Clone)]
pub struct Script {
    pub status: u16,
    pub reason: &'static str,
    pub body: Vec<u8>,
    pub delay_ms: u64,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            status: 200,
            reason: "OK",
            body: Vec::new(),
            delay_ms: 0,
        }
    }
}

/// T040: loopback-only fake HTTP provider. Serves the current [`Script`] to
/// every connection after `delay_ms`; captures raw request bytes per
/// connection for assertions. Test tooling: it never dials out, and the
/// accept loop runs on a detached thread.
pub struct ProviderRig {
    base_url: String,
    script: Arc<Mutex<Script>>,
    captured: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl ProviderRig {
    pub fn spawn() -> std::io::Result<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let script = Arc::new(Mutex::new(Script::default()));
        let captured = Arc::new(Mutex::new(Vec::new()));
        let s = script.clone();
        let c = captured.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(sock) = stream else { continue };
                serve(sock, &s, &c);
            }
        });
        Ok(Self {
            base_url: format!("http://{addr}"),
            script,
            captured,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn set_script(&self, script: Script) {
        *self.script.lock().unwrap() = script;
    }

    /// Drain captured raw requests.
    pub fn take_captured(&self) -> Vec<Vec<u8>> {
        std::mem::take(&mut *self.captured.lock().unwrap())
    }
}

fn serve(
    mut sock: std::net::TcpStream,
    script: &Arc<Mutex<Script>>,
    captured: &Arc<Mutex<Vec<Vec<u8>>>>,
) {
    sock.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    let headers_end = loop {
        match sock.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if let Some(p) = find_header_end(&raw) {
                    break p;
                }
                if raw.len() > 1_048_576 {
                    return;
                }
            }
            Err(_) => return,
        }
    };
    let content_len = content_length(&raw[..headers_end]);
    while raw.len() < headers_end + content_len {
        match sock.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
        if raw.len() > 8_388_608 {
            break;
        }
    }
    captured.lock().unwrap().push(raw);
    let sc = script.lock().unwrap().clone();
    if sc.delay_ms > 0 {
        std::thread::sleep(Duration::from_millis(sc.delay_ms));
    }
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        sc.status,
        sc.reason,
        sc.body.len()
    );
    let _ = sock.write_all(head.as_bytes());
    let _ = sock.write_all(&sc.body);
}

fn find_header_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
}

fn content_length(head: &[u8]) -> usize {
    let text = String::from_utf8_lossy(head);
    text.lines()
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

/// Synthetic secret value planted in fake-provider output. It must never
/// surface in adapter diagnostics.
const CANARY: &str = "sk-canary-probe-9f8e7d6c5b4a2e1f0";

/// Invalid-JSON body with the canary placed past the 512-char diagnostic
/// bound: a contract-compliant adapter drops it; a leaky one is caught.
fn canary_body() -> Vec<u8> {
    let mut b = b"{\"incomplete\": \"".to_vec();
    b.extend(std::iter::repeat_n(b'z', 700));
    b.extend_from_slice(CANARY.as_bytes());
    b
}

fn probe_model() -> ModelId {
    ModelId::new("harness-probe").expect("static model id")
}

fn base_request(timeout_ms: u64) -> InferenceRequest {
    InferenceRequest {
        prompt: "conformance probe".to_string(),
        model: probe_model(),
        max_tokens: Some(8),
        structured_output: false,
        timeout_ms,
        // Probes carry a credential reference the way real dispatches do;
        // adapters that mandate one (e.g. Gemini's key header) need it, and
        // adapters that don't need it must ignore it without leaking it.
        credential: Some(SecretRef::new("harness-probe-cred").expect("static ref")),
    }
}

/// T040: the conformance harness itself.
pub struct Harness {
    config: HarnessConfig,
}

impl Harness {
    pub fn new(config: HarnessConfig) -> Self {
        Self { config }
    }

    /// Run the five conformance checks against `adapter`. The rig must already
    /// be spawned; each check scripts it before invoking the adapter.
    pub async fn run(
        &self,
        adapter: &dyn InferenceAdapter,
        shape: WireShape,
        rig: &ProviderRig,
    ) -> ConformanceReport {
        let name = adapter.endpoint_id().to_string();
        rig.set_script(Script {
            body: shape.ok_body(),
            delay_ms: self.config.slow_provider_delay_ms,
            ..Default::default()
        });
        let timeout = self.check_timeout(adapter).await;
        rig.set_script(Script {
            body: b"{not valid json".to_vec(),
            ..Default::default()
        });
        let malformed = self.check_malformed(adapter).await;
        let cancellation = self.check_cancellation(adapter).await;
        rig.set_script(Script {
            body: shape.ok_body(),
            ..Default::default()
        });
        let usage = self.check_usage(adapter).await;
        rig.set_script(Script {
            body: canary_body(),
            ..Default::default()
        });
        let redaction = self.check_secret_redaction(adapter).await;
        ConformanceReport {
            adapter: name,
            results: vec![timeout, malformed, cancellation, usage, redaction],
        }
    }

    /// The provider hangs; the adapter must surface a retryable Timeout
    /// within the request deadline plus slack.
    async fn check_timeout(&self, adapter: &dyn InferenceAdapter) -> CheckResult {
        let req = base_request(self.config.timeout_check_ms);
        let token = CancellationToken::new();
        let bound = Duration::from_millis(self.config.timeout_check_ms + self.config.timeout_slack_ms);
        let start = Instant::now();
        let outcome = tokio::time::timeout(bound, adapter.infer(req, &token)).await;
        let elapsed = start.elapsed();
        match outcome {
            Err(_) => CheckResult {
                check: "timeout",
                outcome: Outcome::Fail,
                evidence: format!(
                    "adapter ignored the {}ms request deadline (harness outer bound {:?} hit after {:?})",
                    self.config.timeout_check_ms, bound, elapsed
                ),
            },
            Ok(Err(e)) if e.kind == AdapterErrorKind::Timeout => {
                let in_time =
                    elapsed.as_millis() <= (self.config.timeout_check_ms + self.config.timeout_slack_ms) as u128;
                CheckResult {
                    check: "timeout",
                    outcome: if in_time { Outcome::Pass } else { Outcome::Fail },
                    evidence: format!(
                        "timeout error after {:?} (deadline {}ms, retryable={})",
                        elapsed, self.config.timeout_check_ms, e.retryable
                    ),
                }
            }
            Ok(Err(e)) => CheckResult {
                check: "timeout",
                outcome: Outcome::Fail,
                evidence: format!(
                    "expected Timeout, got {:?} (retryable={}): {}",
                    e.kind, e.retryable, e.safe_detail
                ),
            },
            Ok(Ok(_)) => CheckResult {
                check: "timeout",
                outcome: Outcome::Fail,
                evidence: "adapter returned success despite a hung provider".to_string(),
            },
        }
    }

    /// The provider returns non-JSON bytes; the adapter must surface a
    /// non-retryable MalformedResponse.
    async fn check_malformed(&self, adapter: &dyn InferenceAdapter) -> CheckResult {
        let req = base_request(self.config.request_timeout_ms);
        let token = CancellationToken::new();
        let bound = Duration::from_millis(self.config.request_timeout_ms + 5000);
        match tokio::time::timeout(bound, adapter.infer(req, &token)).await {
            Err(_) => CheckResult {
                check: "malformed_response",
                outcome: Outcome::Fail,
                evidence: "harness outer bound hit waiting for malformed-response handling".to_string(),
            },
            Ok(Err(e)) if e.kind == AdapterErrorKind::MalformedResponse && !e.retryable => {
                CheckResult {
                    check: "malformed_response",
                    outcome: Outcome::Pass,
                    evidence: format!(
                        "non-retryable MalformedResponse, detail bounded to {} chars",
                        e.safe_detail.len()
                    ),
                }
            }
            Ok(Err(e)) => CheckResult {
                check: "malformed_response",
                outcome: Outcome::Fail,
                evidence: format!(
                    "expected non-retryable MalformedResponse, got {:?} (retryable={}): {}",
                    e.kind, e.retryable, e.safe_detail
                ),
            },
            Ok(Ok(_)) => CheckResult {
                check: "malformed_response",
                outcome: Outcome::Fail,
                evidence: "adapter accepted invalid JSON as a valid provider response".to_string(),
            },
        }
    }

    /// A pre-cancelled token must yield a Cancelled error before any I/O.
    async fn check_cancellation(&self, adapter: &dyn InferenceAdapter) -> CheckResult {
        let req = base_request(self.config.request_timeout_ms);
        let token = CancellationToken::new();
        token.cancel();
        let bound = Duration::from_millis(5000);
        match tokio::time::timeout(bound, adapter.infer(req, &token)).await {
            Err(_) => CheckResult {
                check: "cancellation",
                outcome: Outcome::Fail,
                evidence: "adapter performed I/O despite a pre-cancelled token (outer bound hit)"
                    .to_string(),
            },
            Ok(Err(e)) if e.kind == AdapterErrorKind::Cancelled => CheckResult {
                check: "cancellation",
                outcome: Outcome::Pass,
                evidence: "pre-cancelled token produced Cancelled before dispatch".to_string(),
            },
            Ok(Err(e)) => CheckResult {
                check: "cancellation",
                outcome: Outcome::Fail,
                evidence: format!(
                    "expected Cancelled, got {:?}: {}",
                    e.kind, e.safe_detail
                ),
            },
            Ok(Ok(_)) => CheckResult {
                check: "cancellation",
                outcome: Outcome::Fail,
                evidence: "adapter returned success despite a pre-cancelled token".to_string(),
            },
        }
    }

    /// Provider-reported usage (10 in / 20 out) must be passed through intact.
    async fn check_usage(&self, adapter: &dyn InferenceAdapter) -> CheckResult {
        let req = base_request(self.config.request_timeout_ms);
        let token = CancellationToken::new();
        let bound = Duration::from_millis(self.config.request_timeout_ms + 5000);
        let want = InferenceUsage {
            input_tokens: 10,
            output_tokens: 20,
        };
        match tokio::time::timeout(bound, adapter.infer(req, &token)).await {
            Err(_) => CheckResult {
                check: "usage_accounting",
                outcome: Outcome::Fail,
                evidence: "harness outer bound hit waiting for usage-bearing response".to_string(),
            },
            Ok(Err(e)) => CheckResult {
                check: "usage_accounting",
                outcome: Outcome::Fail,
                evidence: format!("inference failed: {:?}: {}", e.kind, e.safe_detail),
            },
            Ok(Ok(resp)) => match resp.usage {
                Some(u) if u == want => CheckResult {
                    check: "usage_accounting",
                    outcome: Outcome::Pass,
                    evidence: format!("usage passed through intact: {u:?}"),
                },
                Some(u) => CheckResult {
                    check: "usage_accounting",
                    outcome: Outcome::Fail,
                    evidence: format!("usage mismatch: got {u:?}, want {want:?}"),
                },
                None => CheckResult {
                    check: "usage_accounting",
                    outcome: Outcome::Fail,
                    evidence: "adapter dropped provider-reported usage".to_string(),
                },
            },
        }
    }

    /// A synthetic secret planted past the diagnostic bound in provider
    /// output must not appear in the adapter's surfaced diagnostics, and the
    /// full prompt must not leak there either.
    async fn check_secret_redaction(&self, adapter: &dyn InferenceAdapter) -> CheckResult {
        let prompt = format!("redaction-probe-prompt-{}", "p".repeat(96));
        let req = InferenceRequest {
            prompt: prompt.clone(),
            credential: Some(SecretRef::new("synthetic-probe-cred").expect("static ref")),
            ..base_request(self.config.request_timeout_ms)
        };
        let token = CancellationToken::new();
        let bound = Duration::from_millis(self.config.request_timeout_ms + 5000);
        match tokio::time::timeout(bound, adapter.infer(req, &token)).await {
            Err(_) => CheckResult {
                check: "secret_redaction",
                outcome: Outcome::Fail,
                evidence: "harness outer bound hit waiting for canary-bearing response".to_string(),
            },
            Ok(Ok(_)) => CheckResult {
                check: "secret_redaction",
                outcome: Outcome::Fail,
                evidence: "adapter returned success for the canary (invalid-JSON) body".to_string(),
            },
            Ok(Err(e)) => {
                let debug = format!("{e:?}");
                let mut problems = Vec::new();
                if e.safe_detail.contains(CANARY) {
                    problems.push("safe_detail contains the planted canary");
                }
                if debug.contains(CANARY) {
                    problems.push("Debug output contains the planted canary");
                }
                if e.safe_detail.len() > 640 {
                    problems.push("safe_detail exceeds the 512-char diagnostic bound");
                }
                if e.safe_detail.contains(&prompt) {
                    problems.push("full prompt leaked into diagnostics");
                }
                if problems.is_empty() {
                    CheckResult {
                        check: "secret_redaction",
                        outcome: Outcome::Pass,
                        evidence: format!(
                            "canary absent from diagnostics; detail bounded to {} chars; prompt not leaked",
                            e.safe_detail.len()
                        ),
                    }
                } else {
                    CheckResult {
                        check: "secret_redaction",
                        outcome: Outcome::Fail,
                        evidence: problems.join("; "),
                    }
                }
            }
        }
    }
}

/// T040: run the five conformance checks against `adapter` with `config`.
pub async fn run_conformance(
    adapter: &dyn InferenceAdapter,
    shape: WireShape,
    rig: &ProviderRig,
    config: &HarnessConfig,
) -> ConformanceReport {
    Harness::new(config.clone()).run(adapter, shape, rig).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::AdapterError;
    use crate::gemini::{GeminiAdapter, GeminiConfig};
    use crate::http::CredentialResolver;
    use crate::local::{LocalAdapter, LocalAdapterConfig};
    use crate::nvidia::{NvidiaAdapter, NvidiaSettings};
    use crate::openai::{OpenAiAdapter, OpenAiConfig};
    use crate::traits::InferenceResponse;
    use async_trait::async_trait;
    use fabric_types::EndpointId;
    use std::sync::Arc;

    // ------------------------------------------------------------------
    // ScriptedAdapter: synthetic test double with scripted provider modes.
    // Proves the harness discriminates conforming from non-conforming
    // behavior (negative controls), independent of any HTTP stack.
    // ------------------------------------------------------------------

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ScriptMode {
        /// Contract-correct: honors deadline and cancellation.
        Success,
        /// Provider hangs; adapter enforces the request deadline.
        SlowProvider,
        /// Provider returns garbage; adapter reports MalformedResponse.
        MalformedProvider,
        /// Negative control: ignores deadline AND cancellation.
        Hang,
        /// Negative control: leaks the canary into the error detail.
        LeakyError,
    }

    struct ScriptedAdapter {
        mode: ScriptMode,
        endpoint_id: EndpointId,
    }

    impl ScriptedAdapter {
        fn new(mode: ScriptMode) -> Self {
            Self {
                mode,
                endpoint_id: EndpointId::new("scripted-test").unwrap(),
            }
        }
    }

    #[async_trait]
    impl InferenceAdapter for ScriptedAdapter {
        async fn infer(
            &self,
            req: InferenceRequest,
            cancel: &CancellationToken,
        ) -> Result<InferenceResponse, AdapterError> {
            if cancel.is_cancelled() {
                return Err(AdapterError::fatal(
                    AdapterErrorKind::Cancelled,
                    "scripted: cancelled before dispatch",
                ));
            }
            let deadline = Duration::from_millis(req.timeout_ms.max(1));
            match self.mode {
                ScriptMode::Success => Ok(InferenceResponse {
                    request_id: "scripted-1".to_string(),
                    raw_body: r#"{"ok":true}"#.to_string(),
                    usage: Some(InferenceUsage {
                        input_tokens: 10,
                        output_tokens: 20,
                    }),
                    finish_reason: Some("stop".to_string()),
                }),
                ScriptMode::SlowProvider => {
                    match tokio::time::timeout(deadline, tokio::time::sleep(Duration::from_secs(3600)))
                        .await
                    {
                        Ok(_) => Err(AdapterError::fatal(
                            AdapterErrorKind::Unknown,
                            "scripted sleep unexpectedly finished",
                        )),
                        Err(_) => Err(AdapterError::retryable(
                            AdapterErrorKind::Timeout,
                            "scripted provider exceeded request deadline",
                        )),
                    }
                }
                ScriptMode::MalformedProvider => Err(AdapterError::fatal(
                    AdapterErrorKind::MalformedResponse,
                    "scripted provider returned non-JSON bytes",
                )),
                ScriptMode::Hang => {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    Err(AdapterError::fatal(
                        AdapterErrorKind::Unknown,
                        "scripted hang finished",
                    ))
                }
                ScriptMode::LeakyError => Err(AdapterError::fatal(
                    AdapterErrorKind::MalformedResponse,
                    format!("scripted parse failure; raw tail: {CANARY}"),
                )),
            }
        }

        fn endpoint_id(&self) -> &EndpointId {
            &self.endpoint_id
        }
    }

    fn rig() -> ProviderRig {
        ProviderRig::spawn().expect("loopback rig must bind")
    }

    #[test]
    fn rig_serves_script_and_captures_request() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let rig = rig();
            rig.set_script(Script {
                body: b"hello".to_vec(),
                ..Default::default()
            });
            let body = reqwest::get(rig.base_url()).await.unwrap().bytes().await.unwrap();
            assert_eq!(&body[..], b"hello");
            let captured = rig.take_captured();
            assert_eq!(captured.len(), 1);
            assert!(captured[0].starts_with(b"GET / HTTP/1.1"));
        });
    }

    #[test]
    fn rig_honors_delay() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let rig = rig();
            rig.set_script(Script {
                body: b"x".to_vec(),
                delay_ms: 400,
                ..Default::default()
            });
            let start = Instant::now();
            reqwest::get(rig.base_url()).await.unwrap().bytes().await.unwrap();
            assert!(start.elapsed() >= Duration::from_millis(400));
        });
    }

    #[tokio::test]
    async fn harness_passes_conforming_scripted_adapter() {
        let rig = rig();
        let h = Harness::new(HarnessConfig::default());
        // Each check needs its matching scripted mode.
        let t = h.check_timeout(&ScriptedAdapter::new(ScriptMode::SlowProvider)).await;
        assert_eq!(t.outcome, Outcome::Pass, "{}", t.evidence);
        let m = h.check_malformed(&ScriptedAdapter::new(ScriptMode::MalformedProvider)).await;
        assert_eq!(m.outcome, Outcome::Pass, "{}", m.evidence);
        let c = h.check_cancellation(&ScriptedAdapter::new(ScriptMode::Success)).await;
        assert_eq!(c.outcome, Outcome::Pass, "{}", c.evidence);
        let u = h.check_usage(&ScriptedAdapter::new(ScriptMode::Success)).await;
        assert_eq!(u.outcome, Outcome::Pass, "{}", u.evidence);
        // Redaction check needs the canary script; the scripted adapter
        // returns a bounded error, so the canary (only in the rig body) is absent.
        rig.set_script(Script { body: canary_body(), ..Default::default() });
        let r = h.check_secret_redaction(&ScriptedAdapter::new(ScriptMode::MalformedProvider)).await;
        assert_eq!(r.outcome, Outcome::Pass, "{}", r.evidence);
    }

    #[tokio::test]
    async fn harness_catches_deadline_ignoring_adapter() {
        // Negative control: Hang ignores the deadline; the harness must FAIL it.
        let h = Harness::new(HarnessConfig::default());
        let t = h.check_timeout(&ScriptedAdapter::new(ScriptMode::Hang)).await;
        assert_eq!(t.outcome, Outcome::Fail, "{}", t.evidence);
        assert!(t.evidence.contains("ignored"));
    }

    #[tokio::test]
    async fn harness_catches_canary_leak() {
        // Negative control: LeakyError embeds the canary in the detail.
        let h = Harness::new(HarnessConfig::default());
        let r = h.check_secret_redaction(&ScriptedAdapter::new(ScriptMode::LeakyError)).await;
        assert_eq!(r.outcome, Outcome::Fail, "{}", r.evidence);
        assert!(r.evidence.contains("canary"));
    }

    #[tokio::test]
    async fn harness_catches_dropped_usage() {
        struct NoUsage(ScriptedAdapter);
        #[async_trait]
        impl InferenceAdapter for NoUsage {
            async fn infer(
                &self,
                req: InferenceRequest,
                cancel: &CancellationToken,
            ) -> Result<InferenceResponse, AdapterError> {
                let mut r = self.0.infer(req, cancel).await?;
                r.usage = None;
                Ok(r)
            }
            fn endpoint_id(&self) -> &EndpointId {
                self.0.endpoint_id()
            }
        }
        let h = Harness::new(HarnessConfig::default());
        let u = h.check_usage(&NoUsage(ScriptedAdapter::new(ScriptMode::Success))).await;
        assert_eq!(u.outcome, Outcome::Fail, "{}", u.evidence);
        assert!(u.evidence.contains("dropped"));
    }

    #[test]
    fn report_counters_and_green() {
        let rep = ConformanceReport {
            adapter: "x".to_string(),
            results: vec![
                CheckResult { check: "a", outcome: Outcome::Pass, evidence: String::new() },
                CheckResult { check: "b", outcome: Outcome::Fail, evidence: String::new() },
                CheckResult { check: "c", outcome: Outcome::Inconclusive, evidence: String::new() },
            ],
        };
        assert_eq!((rep.passed(), rep.failed(), rep.inconclusive()), (1, 1, 1));
        assert!(!rep.is_green());
        let json = serde_json::to_string(&rep).unwrap();
        assert!(json.contains("INCONCLUSIVE"));
    }

    // ------------------------------------------------------------------
    // Required gate: run the battery against the crate's real adapters,
    // steered via the loopback rig. No live network.
    // ------------------------------------------------------------------

    struct GateResolver;
    impl CredentialResolver for GateResolver {
        fn bearer_token(&self, _r: &SecretRef) -> Result<String, AdapterError> {
            Ok("sk-synth-gate-TESTONLY-9f8e7d".to_string())
        }
    }

    #[tokio::test]
    async fn conformance_gate_real_adapters() {
        let rig = rig();
        let config = HarnessConfig::default();
        let resolver: Arc<dyn CredentialResolver> = Arc::new(GateResolver);
        let model = || ModelId::new("gate-model").unwrap();

        let local = LocalAdapter::new(
            LocalAdapterConfig::new(rig.base_url(), EndpointId::new("gate-local").unwrap(), model())
                .unwrap(),
        )
        .unwrap();
        let openai = OpenAiAdapter::new(
            EndpointId::new("gate-openai").unwrap(),
            OpenAiConfig::new(rig.base_url(), model()).unwrap(),
            resolver.clone(),
        )
        .unwrap();
        let nvidia = NvidiaAdapter::new(
            EndpointId::new("gate-nvidia").unwrap(),
            NvidiaSettings {
                base_url: Some(rig.base_url().to_string()),
                model: model(),
            },
            None,
            resolver.clone(),
        )
        .unwrap();
        let mut gcfg = GeminiConfig::new(model());
        gcfg.base_url = Some(rig.base_url().to_string());
        let gemini = GeminiAdapter::new(
            EndpointId::new("gate-gemini").unwrap(),
            gcfg,
            resolver.clone(),
        )
        .unwrap();

        let cases: Vec<(&dyn InferenceAdapter, WireShape)> = vec![
            (&local, WireShape::OpenAiChatCompletions),
            (&openai, WireShape::OpenAiChatCompletions),
            (&nvidia, WireShape::OpenAiChatCompletions),
            (&gemini, WireShape::GeminiGenerateContent),
        ];
        for (adapter, shape) in cases {
            let report = run_conformance(adapter, shape, &rig, &config).await;
            println!(
                "conformance report:\n{}",
                serde_json::to_string_pretty(&report).unwrap()
            );
            assert!(
                report.is_green(),
                "conformance gate failed for {}: {:?}",
                report.adapter, report.results
            );
            assert_eq!(report.results.len(), 5);
        }
    }
}
