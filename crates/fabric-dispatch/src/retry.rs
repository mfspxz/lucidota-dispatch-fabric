//! T064: bounded retries with retry classes, backoff, and injectable jitter.
//!
//! Retry decisions are explicit and bounded: an error is classified as
//! retryable or not, the [`RetryPolicy`] bounds attempts, a separate retry
//! budget bounds total retries, and backoff delays use full jitter from an
//! injectable deterministic RNG so tests never depend on wall-clock randomness.

use fabric_types::{BackoffKind, FabricError, RetryPolicy};

/// Whether a failed attempt may be retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryClass {
    /// Safe to try again: timeouts, transport failures, quota exhaustion.
    Retryable,
    /// Trying again without a changed input will not help: policy denials,
    /// malformed output, conflicts, verification failures, invalid input.
    NonRetryable,
}

/// Classify a structured error into a retry class.
pub fn classify_error(err: &FabricError) -> RetryClass {
    match err {
        FabricError::Timeout(_)
        | FabricError::TransportFailure(_)
        | FabricError::QuotaExhausted(_) => RetryClass::Retryable,
        _ => RetryClass::NonRetryable,
    }
}

/// Deterministic xorshift64* RNG for jitter. The seed is injected by the
/// caller, so retry delays are reproducible in tests and replays.
#[derive(Debug, Clone)]
pub struct JitterRng {
    state: u64,
}

impl JitterRng {
    pub fn new(seed: u64) -> Self {
        // Zero is a degenerate xorshift state; map it to a fixed nonzero seed.
        Self {
            state: if seed == 0 { 0x9E3779B97F4A7C15 } else { seed },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    /// Uniform value in `[0, bound)`. `bound == 0` yields 0.
    pub fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            return 0;
        }
        // Multiply-high method: unbiased enough for jitter, no modulo bias trap.
        ((self.next_u64() as u128 * bound as u128) >> 64) as u64
    }
}

/// Backoff delay for the retry following `failed_attempt` (1-based), with
/// full jitter: uniform in `[0, backoff(failed_attempt)]`.
///
/// - None: always 0.
/// - Linear: `base_delay_ms * failed_attempt`.
/// - Exponential: `base_delay_ms * 2^(failed_attempt-1)`, saturating.
pub fn backoff_delay_ms(
    policy: &RetryPolicy,
    failed_attempt: u32,
    jitter: &mut JitterRng,
) -> u64 {
    let base = policy.base_delay_ms;
    let nominal = match policy.backoff {
        BackoffKind::None => 0,
        BackoffKind::Linear => base.saturating_mul(failed_attempt.max(1) as u64),
        BackoffKind::Exponential => {
            let shift = failed_attempt.saturating_sub(1).min(62);
            base.saturating_mul(1u64 << shift)
        }
    };
    if nominal == 0 {
        return 0;
    }
    jitter.below(nominal.saturating_add(1))
}

/// The retry decision for one failed attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    /// Try again as `next_attempt` after `delay_ms`.
    Retry { delay_ms: u64, next_attempt: u32 },
    /// Do not retry. `reason` is a stable machine-readable code.
    GiveUp { reason: &'static str },
}

/// Decide whether to retry after `failed_attempt` (1-based) failed with `err`.
///
/// `budget_remaining` is decremented on every granted retry; when it hits
/// zero, no further retries are granted even if attempts remain. This keeps
/// retry storms bounded independently of the per-task attempt cap.
pub fn should_retry(
    policy: &RetryPolicy,
    failed_attempt: u32,
    budget_remaining: &mut u32,
    err: &FabricError,
    jitter: &mut JitterRng,
) -> RetryDecision {
    if classify_error(err) != RetryClass::Retryable {
        return RetryDecision::GiveUp {
            reason: "NON_RETRYABLE",
        };
    }
    if failed_attempt >= policy.max_attempts {
        return RetryDecision::GiveUp {
            reason: "MAX_ATTEMPTS",
        };
    }
    if *budget_remaining == 0 {
        return RetryDecision::GiveUp {
            reason: "RETRY_BUDGET_EXHAUSTED",
        };
    }
    *budget_remaining -= 1;
    RetryDecision::Retry {
        delay_ms: backoff_delay_ms(policy, failed_attempt, jitter),
        next_attempt: failed_attempt + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> RetryPolicy {
        RetryPolicy::new(3, BackoffKind::Exponential, 100, 10).unwrap()
    }

    #[test]
    fn classifies_error_kinds() {
        assert_eq!(
            classify_error(&FabricError::Timeout("x".into())),
            RetryClass::Retryable
        );
        assert_eq!(
            classify_error(&FabricError::TransportFailure("x".into())),
            RetryClass::Retryable
        );
        assert_eq!(
            classify_error(&FabricError::QuotaExhausted("x".into())),
            RetryClass::Retryable
        );
        assert_eq!(
            classify_error(&FabricError::PolicyDenied("x".into())),
            RetryClass::NonRetryable
        );
        assert_eq!(
            classify_error(&FabricError::MalformedOutput("x".into())),
            RetryClass::NonRetryable
        );
        assert_eq!(
            classify_error(&FabricError::VerificationFailure("x".into())),
            RetryClass::NonRetryable
        );
        assert_eq!(
            classify_error(&FabricError::Conflict("x".into())),
            RetryClass::NonRetryable
        );
    }

    #[test]
    fn grants_retry_with_backoff_and_decrements_budget() {
        let p = policy();
        let mut budget = 10u32;
        let mut j = JitterRng::new(42);
        match should_retry(&p, 1, &mut budget, &FabricError::Timeout("t".into()), &mut j) {
            RetryDecision::Retry { delay_ms, next_attempt } => {
                assert_eq!(next_attempt, 2);
                assert!(delay_ms <= 100); // full jitter within [0, 100]
            }
            d => panic!("unexpected {d:?}"),
        }
        assert_eq!(budget, 9);
    }

    #[test]
    fn gives_up_on_non_retryable_max_attempts_and_budget() {
        let p = policy();
        let mut j = JitterRng::new(1);
        let mut b = 10u32;
        assert_eq!(
            should_retry(&p, 1, &mut b, &FabricError::PolicyDenied("x".into()), &mut j),
            RetryDecision::GiveUp { reason: "NON_RETRYABLE" }
        );
        let mut b = 10u32;
        assert_eq!(
            should_retry(&p, 3, &mut b, &FabricError::Timeout("x".into()), &mut j),
            RetryDecision::GiveUp { reason: "MAX_ATTEMPTS" }
        );
        let mut b = 0u32;
        assert_eq!(
            should_retry(&p, 1, &mut b, &FabricError::Timeout("x".into()), &mut j),
            RetryDecision::GiveUp { reason: "RETRY_BUDGET_EXHAUSTED" }
        );
    }

    #[test]
    fn backoff_shapes_are_correct() {
        let mut j = JitterRng::new(7);
        let lin = RetryPolicy::new(5, BackoffKind::Linear, 100, 5).unwrap();
        // Linear attempt 3: nominal 300, jitter in [0,300]
        assert!(backoff_delay_ms(&lin, 3, &mut j) <= 300);
        let none = RetryPolicy::new(5, BackoffKind::None, 100, 5).unwrap();
        assert_eq!(backoff_delay_ms(&none, 3, &mut j), 0);
        // Exponential saturates instead of overflowing (debug builds would
        // panic on overflow; saturation keeps this total).
        let exp = RetryPolicy::new(64, BackoffKind::Exponential, u64::MAX, 64).unwrap();
        let mut ja = JitterRng::new(777);
        let mut jb = JitterRng::new(777);
        let d = backoff_delay_ms(&exp, 64, &mut ja);
        let d2 = backoff_delay_ms(&exp, 64, &mut jb);
        assert_eq!(d, d2);
    }

    #[test]
    fn jitter_is_deterministic_per_seed() {
        let mut a = JitterRng::new(99);
        let mut b = JitterRng::new(99);
        for _ in 0..100 {
            assert_eq!(a.below(1000), b.below(1000));
        }
        let mut c = JitterRng::new(100);
        assert_ne!(a.below(1_000_000), c.below(1_000_000));
    }
}
