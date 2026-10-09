//! T043: exact bounded sliding-window request-rate limiter.
//!
//! Exact: every admission decision is computed from the true set of event
//! timestamps inside the window — no probabilistic counting, no fixed-window
//! boundary artifacts. Bounded: at most `max_requests` timestamps are ever
//! retained, so memory is O(max_requests). The window slides per event: an
//! event at time `t` counts toward the window `(now - window_ms, now]`.

use crate::limits::{LimitKind, LimitSet};
use crate::UnixMillis;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Exact sliding-window rate limiter over an injectable clock.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlidingWindow {
    window_ms: u64,
    max_requests: u64,
    /// Timestamps of admitted requests, ascending. Only timestamps within
    /// the current window are retained (bounded by max_requests).
    events: VecDeque<UnixMillis>,
}

impl SlidingWindow {
    /// Create a limiter allowing `max_requests` per `window_ms`.
    /// Returns `None` for degenerate parameters (never invent a limit).
    pub fn new(window_ms: u64, max_requests: u64) -> Option<Self> {
        if window_ms == 0 || max_requests == 0 {
            return None;
        }
        Some(Self {
            window_ms,
            max_requests,
            events: VecDeque::new(),
        })
    }

    /// Derive from a [`LimitSet`]'s request-rate dimension. `None` when the
    /// dimension is unknown, windowless, or degenerate.
    pub fn from_limits(limits: &LimitSet) -> Option<Self> {
        let q = limits.get(LimitKind::RequestRate)?;
        Self::new(q.window_ms?, q.max?)
    }

    /// Drop events that have fallen out of the window ending at `now_ms`.
    /// An event at `t` belongs to the window iff `t + window_ms > now_ms`;
    /// the addition form avoids underflow when `now_ms < window_ms`.
    fn evict(&mut self, now_ms: UnixMillis) {
        while self
            .events
            .front()
            .is_some_and(|t| t.saturating_add(self.window_ms) <= now_ms)
        {
            self.events.pop_front();
        }
    }

    /// Attempt to admit one request at `now_ms`. Admitted requests are
    /// recorded; denied requests leave no trace.
    pub fn try_acquire(&mut self, now_ms: UnixMillis) -> bool {
        self.evict(now_ms);
        if self.events.len() as u64 >= self.max_requests {
            return false;
        }
        self.events.push_back(now_ms);
        // Bounded invariant: eviction above guarantees len <= max_requests,
        // but a backwards clock could in theory retain more; enforce the cap.
        while self.events.len() as u64 > self.max_requests {
            self.events.pop_front();
        }
        true
    }

    /// Number of admitted requests inside the window ending at `now_ms`.
    /// Non-mutating: counts without recording.
    pub fn count(&self, now_ms: UnixMillis) -> u64 {
        self.events
            .iter()
            .filter(|t| t.saturating_add(self.window_ms) > now_ms)
            .count() as u64
    }

    /// Retained event count (<= max_requests by construction).
    pub fn retained(&self) -> usize {
        self.events.len()
    }

    pub fn window_ms(&self) -> u64 {
        self.window_ms
    }

    pub fn max_requests(&self) -> u64 {
        self.max_requests
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_up_to_max_then_denies() {
        let mut w = SlidingWindow::new(1000, 3).unwrap();
        assert!(w.try_acquire(0));
        assert!(w.try_acquire(100));
        assert!(w.try_acquire(200));
        assert!(!w.try_acquire(300));
        assert_eq!(w.count(300), 3);
    }

    #[test]
    fn window_slides_per_event_no_boundary_artifact() {
        let mut w = SlidingWindow::new(1000, 2).unwrap();
        assert!(w.try_acquire(0));
        assert!(w.try_acquire(10));
        assert!(!w.try_acquire(20));
        // At t=1001 the event at t=0 falls out of (1, 1001]: admitted.
        assert!(w.try_acquire(1001));
        // Now window (1,1001] holds t=10 and t=1001: full again.
        assert!(!w.try_acquire(1002));
        // At t=1011 the t=10 event expires: admitted.
        assert!(w.try_acquire(1011));
    }

    #[test]
    fn denied_requests_leave_no_trace() {
        let mut w = SlidingWindow::new(1000, 1).unwrap();
        assert!(w.try_acquire(0));
        assert!(!w.try_acquire(500));
        assert_eq!(w.retained(), 1);
        // The denial at t=500 must not block t=1001.
        assert!(w.try_acquire(1001));
    }

    #[test]
    fn memory_is_bounded() {
        let mut w = SlidingWindow::new(60_000, 5).unwrap();
        for t in (0..1000).map(|i| i * 10) {
            w.try_acquire(t);
        }
        assert!(w.retained() <= 5);
    }

    #[test]
    fn count_does_not_mutate() {
        let mut w = SlidingWindow::new(1000, 10).unwrap();
        w.try_acquire(0);
        w.try_acquire(100);
        assert_eq!(w.count(500), 2);
        assert_eq!(w.retained(), 2);
        assert_eq!(w.count(2000), 0); // both expired, still retained until evict
    }

    #[test]
    fn rejects_degenerate_construction() {
        assert!(SlidingWindow::new(0, 5).is_none());
        assert!(SlidingWindow::new(1000, 0).is_none());
    }

    #[test]
    fn from_limits_uses_request_rate_dimension() {
        let mut limits = LimitSet::new();
        assert!(SlidingWindow::from_limits(&limits).is_none()); // unknown
        limits.set(crate::limits::QuotaLimit {
            kind: LimitKind::RequestRate,
            max: Some(2),
            window_ms: Some(1000),
        });
        let mut w = SlidingWindow::from_limits(&limits).unwrap();
        assert!(w.try_acquire(0));
        assert!(w.try_acquire(0));
        assert!(!w.try_acquire(0));
    }

    #[test]
    fn decisions_are_reproducible() {
        let run = || {
            let mut w = SlidingWindow::new(1000, 3).unwrap();
            [0u64, 100, 200, 300, 1100, 1200, 1300, 1400]
                .iter()
                .map(|t| w.try_acquire(*t))
                .collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
    }
}
