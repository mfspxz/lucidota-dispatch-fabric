//! Provider adapters (T031-T039).
//!
//! Async provider-neutral inference trait, configurable HTTP transport,
//! OpenAI-compatible / NVIDIA / Gemini / local adapters, a Copilot
//! integration boundary, response normalization, and typed error
//! normalization.
//!
//! Interface contracts frozen by the orchestrator (PHASE 0): `traits`,
//! `errors`. Workers implement bodies and own the remaining modules outright.

pub mod copilot;
pub mod errors;
pub mod gemini;
pub mod http;
pub mod local;
pub mod normalize;
pub mod nvidia;
pub mod openai;
pub mod traits;

pub use errors::{AdapterError, AdapterErrorKind};
pub use traits::{CancellationToken, InferenceAdapter, InferenceRequest, InferenceResponse, InferenceUsage};
