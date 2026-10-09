//! T031 — INTERFACE CONTRACT (frozen by orchestrator).
//!
//! Async provider-neutral inference trait with typed request, response,
//! cancellation, usage, and error structures.
//!
//! Worker A1: implement the adapter structs that satisfy this trait; the
//! trait and these types are frozen.

use crate::errors::AdapterError;
use async_trait::async_trait;
use fabric_types::{EndpointId, ModelId, SecretRef};
use std::sync::atomic::{AtomicBool, Ordering};

/// T031: typed inference request. No provider-specific fields.
#[derive(Debug, Clone)]
pub struct InferenceRequest {
    pub prompt: String,
    pub model: ModelId,
    pub max_tokens: Option<u32>,
    pub structured_output: bool,
    /// Total-response deadline in milliseconds.
    pub timeout_ms: u64,
    /// Credential resolved via secret reference; never a raw key.
    pub credential: Option<SecretRef>,
}

/// T031: token usage reported by the provider, when available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InferenceUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// T031: raw provider response before normalization (see T038).
#[derive(Debug, Clone)]
pub struct InferenceResponse {
    pub request_id: String,
    pub raw_body: String,
    pub usage: Option<InferenceUsage>,
    pub finish_reason: Option<String>,
}

/// T031: cooperative cancellation token shared with in-flight inference.
#[derive(Debug, Default)]
pub struct CancellationToken {
    flag: AtomicBool,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

/// T031: provider-neutral async inference. Implemented per provider in
/// `openai`, `nvidia`, `gemini`, `local`; bounded by `copilot`.
#[async_trait]
pub trait InferenceAdapter: Send + Sync {
    async fn infer(
        &self,
        req: InferenceRequest,
        cancel: &CancellationToken,
    ) -> Result<InferenceResponse, AdapterError>;

    fn endpoint_id(&self) -> &EndpointId;
}
