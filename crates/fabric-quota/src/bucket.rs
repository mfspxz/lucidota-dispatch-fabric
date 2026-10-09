//! T042: deterministic per-endpoint token-bucket admission control.
//!
//! The clock is injectable: every method takes `now_ms: UnixMillis` and the
//! bucket never reads the system clock. Given the same sequence of
//! (now_ms, requested tokens), the admission decisions are bit-identical,
//! which is what makes scheduling decisions reproducible.

use crate::limits::{LimitKind, LimitSet};
use crate::UnixMillis;
use serde::{Deserialize, Serialize};

/// Admission decision for a token request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Admission {
    Granted,
    Denied {
        /// Milliseconds until `tokens` would be available. `u64::MAX` when the
        /// request can never be satisfied (exceeds capacity).
        retry_after_ms: u64,
    },
}

/// Token bucket. `capacity` is the maximum burst; tokens refill continuously
/// at `refill_per_ms`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBucket {
    capacity: f64,
    refill_per_ms: f64,
    tokens: f64,
    last_ms: UnixMillis,
}

impl TokenBucket {
    /// Create a bucket. `capacity` and `refill_per_second` must be positive.
    pub fn new(capacity: u64, refill_per_second: f64, now_ms: UnixMillis) -> Option<Self> {
        if capacity == 0 || !refill_per_second.is_finite() || refill_per_second <= 0.0 {
            return None;
        }
        Some(Self {
            capacity: capacity as f64,
            refill_per_ms: refill_per_second / 1000.0,
            tokens: capacity as f64,
            last_ms: now_ms,
        })
    }

    /// Derive a bucket from a [`LimitSet`]'s token-rate dimension.
    /// Returns `None` when the token-rate limit is unknown, has no window,
    /// or is otherwise unusable — the caller must treat that as "no bucket",
    /// never invent a rate.
    pub fn from_limits(limits: &LimitSet, now_ms: UnixMillis) -> Option<Self> {
        let q = limits.get(LimitKind::TokenRate)?;
        let max = q.max?;
        let window_ms = q.window_ms?;
        if window_ms == 0 {
            return None;
        }
        let refill_per_second = max as f64 / (window_ms as f64 / 1000.0);
        Self::new(max, refill_per_second, now_ms)
    }

    /// Refill the bucket up to `now_ms`. Clock skew backwards is treated as
    /// "no time passed" rather than an error.
    fn refill(&mut self, now_ms: UnixMillis) {
        let elapsed = now_ms.saturating_sub(self.last_ms);
        self.last_ms = now_ms.max(self.last_ms);
        self.tokens = (self.tokens + elapsed as f64 * self.refill_per_ms).min(self.capacity);
    }

    /// Attempt to consume `tokens`. On success the tokens are deducted.
    pub fn try_acquire(&mut self, tokens: u64, now_ms: UnixMillis) -> Admission {
        self.refill(now_ms);
        let want = tokens as f64;
        if want <= self.tokens {
            self.tokens -= want;
            Admission::Granted
        } else if want > self.capacity {
            Admission::Denied {
                retry_after_ms: u64::MAX,
            }
        } else {
            let deficit = want - self.tokens;
            // refill_per_ms > 0 by construction
            let wait = (deficit / self.refill_per_ms).ceil() as u64;
            Admission::Denied {
                retry_after_ms: wait.max(1),
            }
        }
    }

    /// Tokens currently available (projected to `now_ms`, without mutating).
    pub fn available(&self, now_ms: UnixMillis) -> u64 {
        let elapsed = now_ms.saturating_sub(self.last_ms);
        (self.tokens + elapsed as f64 * self.refill_per_ms)
            .min(self.capacity)
            .floor() as u64
    }

    pub fn capacity(&self) -> u64 {
        self.capacity as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grants_within_capacity_then_denies() {
        let mut b = TokenBucket::new(10, 10.0, 0).unwrap(); // 10 tokens, 10/s
        assert_eq!(b.try_acquire(10, 0), Admission::Granted);
        assert_eq!(b.available(0), 0);
        match b.try_acquire(1, 0) {
            Admission::Denied { retry_after_ms } => assert!(retry_after_ms > 0),
            Admission::Granted => panic!("should deny"),
        }
    }

    #[test]
    fn refills_over_time_deterministically() {
        let mut b = TokenBucket::new(10, 10.0, 0).unwrap();
        assert_eq!(b.try_acquire(10, 0), Admission::Granted);
        // 500ms -> 5 tokens refilled
        assert_eq!(b.available(500), 5);
        assert_eq!(b.try_acquire(5, 500), Admission::Granted);
        // never exceeds capacity
        assert_eq!(b.available(100_000), 10);
    }

    #[test]
    fn never_satisfiable_request_reports_max_wait() {
        let mut b = TokenBucket::new(10, 10.0, 0).unwrap();
        assert_eq!(
            b.try_acquire(11, 0),
            Admission::Denied {
                retry_after_ms: u64::MAX
            }
        );
    }

    #[test]
    fn retry_after_matches_refill_math() {
        let mut b = TokenBucket::new(10, 10.0, 0).unwrap();
        assert_eq!(b.try_acquire(10, 0), Admission::Granted);
        // need 1 token at 10/s -> 100ms
        assert_eq!(
            b.try_acquire(1, 0),
            Admission::Denied { retry_after_ms: 100 }
        );
    }

    #[test]
    fn clock_skew_backwards_does_not_refill_or_panic() {
        let mut b = TokenBucket::new(10, 10.0, 1000).unwrap();
        assert_eq!(b.try_acquire(10, 1000), Admission::Granted);
        // Earlier timestamp: no refill happens, no panic.
        assert_eq!(b.available(500), 0);
        // Backwards request with empty bucket is denied (deterministic),
        // and the bucket recovers on forward time.
        assert!(matches!(
            b.try_acquire(10, 500),
            Admission::Denied { .. }
        ));
        assert_eq!(b.try_acquire(10, 2000), Admission::Granted);
    }

    #[test]
    fn rejects_degenerate_construction() {
        assert!(TokenBucket::new(0, 10.0, 0).is_none());
        assert!(TokenBucket::new(10, 0.0, 0).is_none());
        assert!(TokenBucket::new(10, f64::NAN, 0).is_none());
        assert!(TokenBucket::new(10, f64::INFINITY, 0).is_none());
    }

    #[test]
    fn from_limits_uses_token_rate_dimension() {
        let mut limits = LimitSet::new();
        // unknown token rate -> no bucket, never invented
        assert!(TokenBucket::from_limits(&limits, 0).is_none());
        limits.set(crate::limits::QuotaLimit {
            kind: LimitKind::TokenRate,
            max: Some(600),
            window_ms: Some(60_000),
        });
        let b = TokenBucket::from_limits(&limits, 0).unwrap();
        assert_eq!(b.capacity(), 600);
        // 600 tokens / 60s = 10/s
        let mut b = b;
        assert_eq!(b.try_acquire(600, 0), Admission::Granted);
        assert_eq!(
            b.try_acquire(1, 0),
            Admission::Denied { retry_after_ms: 100 }
        );
    }

    #[test]
    fn from_limits_rejects_windowless_or_zero_window() {
        let mut limits = LimitSet::new();
        limits.set(crate::limits::QuotaLimit {
            kind: LimitKind::TokenRate,
            max: Some(100),
            window_ms: None,
        });
        assert!(TokenBucket::from_limits(&limits, 0).is_none());
        limits.set(crate::limits::QuotaLimit {
            kind: LimitKind::TokenRate,
            max: Some(100),
            window_ms: Some(0),
        });
        assert!(TokenBucket::from_limits(&limits, 0).is_none());
    }

    #[test]
    fn decisions_are_reproducible() {
        let run = || {
            let mut b = TokenBucket::new(5, 2.0, 0).unwrap();
            let seq = [(3u64, 0u64), (3, 100), (1, 600), (5, 2000)];
            seq.iter()
                .map(|(t, now)| b.try_acquire(*t, *now))
                .collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
    }
}
