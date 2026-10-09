//! T024: endpoint health tracking.
//!
//! Tracks last success, recent consecutive failures, cooldown deadline, and a
//! health confidence score per endpoint. Time is always passed in as
//! [`UnixMillis`] (injectable clock) — this module never reads the system
//! clock, so health decisions are deterministic in tests and replays.
//!
//! Confidence model (documented, deterministic):
//! - starts at 0.5 (unknown),
//! - +0.2 per success, capped at 1.0,
//! - -0.25 per failure, floored at 0.0.
//! - after `max_failures` consecutive failures the endpoint enters cooldown
//!   until `now + cooldown_ms`; a success clears failures and cooldown.

use crate::UnixMillis;
use fabric_types::EndpointId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// T024: health record for one endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EndpointHealth {
    pub endpoint: EndpointId,
    pub last_success_millis: Option<UnixMillis>,
    pub recent_failures: u32,
    pub cooldown_until_millis: Option<UnixMillis>,
    /// 0.0..=1.0 confidence the endpoint is currently healthy.
    pub confidence: f64,
}

impl EndpointHealth {
    /// Fresh record: unknown health, confidence 0.5.
    pub fn new(endpoint: EndpointId) -> Self {
        Self {
            endpoint,
            last_success_millis: None,
            recent_failures: 0,
            cooldown_until_millis: None,
            confidence: 0.5,
        }
    }

    /// Record a successful request at `now_millis`.
    pub fn record_success(&mut self, now_millis: UnixMillis) {
        self.last_success_millis = Some(now_millis);
        self.recent_failures = 0;
        self.cooldown_until_millis = None;
        self.confidence = (self.confidence + 0.2).min(1.0);
    }

    /// Record a failed request at `now_millis`. When consecutive failures
    /// reach `max_failures` (minimum 1), the endpoint cools down for
    /// `cooldown_ms`.
    pub fn record_failure(
        &mut self,
        now_millis: UnixMillis,
        max_failures: u32,
        cooldown_ms: UnixMillis,
    ) {
        self.recent_failures = self.recent_failures.saturating_add(1);
        self.confidence = (self.confidence - 0.25).max(0.0);
        if self.recent_failures >= max_failures.max(1) {
            self.cooldown_until_millis = Some(now_millis.saturating_add(cooldown_ms));
        }
    }

    /// True while `now_millis` is before the cooldown deadline.
    pub fn in_cooldown(&self, now_millis: UnixMillis) -> bool {
        self.cooldown_until_millis
            .is_some_and(|until| now_millis < until)
    }

    /// Eligible for dispatch at `now_millis`: not in cooldown.
    pub fn is_healthy(&self, now_millis: UnixMillis) -> bool {
        !self.in_cooldown(now_millis)
    }
}

/// Table of health records keyed by endpoint ID.
#[derive(Debug, Default)]
pub struct HealthTable {
    records: HashMap<EndpointId, EndpointHealth>,
}

impl HealthTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get the record for an endpoint, creating a fresh one on first use.
    pub fn record(&mut self, endpoint: EndpointId) -> &mut EndpointHealth {
        self.records
            .entry(endpoint.clone())
            .or_insert_with(|| EndpointHealth::new(endpoint))
    }

    /// Borrow a record if one exists.
    pub fn get(&self, endpoint: &EndpointId) -> Option<&EndpointHealth> {
        self.records.get(endpoint)
    }

    /// Endpoints currently eligible for dispatch, in deterministic ID order.
    pub fn healthy_endpoints(&self, now_millis: UnixMillis) -> Vec<&EndpointId> {
        let mut out: Vec<&EndpointId> = self
            .records
            .values()
            .filter(|h| h.is_healthy(now_millis))
            .map(|h| &h.endpoint)
            .collect();
        out.sort();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> EndpointId {
        EndpointId::new(s).unwrap()
    }

    #[test]
    fn fresh_record_is_unknown_not_cooling() {
        let h = EndpointHealth::new(id("ep-1"));
        assert_eq!(h.confidence, 0.5);
        assert_eq!(h.last_success_millis, None);
        assert_eq!(h.recent_failures, 0);
        assert!(h.is_healthy(1_000));
    }

    #[test]
    fn success_raises_confidence_and_clears_failures() {
        let mut h = EndpointHealth::new(id("ep-1"));
        h.record_failure(100, 3, 60_000);
        assert_eq!(h.confidence, 0.25);
        h.record_success(200);
        assert_eq!(h.recent_failures, 0);
        assert_eq!(h.last_success_millis, Some(200));
        assert_eq!(h.cooldown_until_millis, None);
        assert_eq!(h.confidence, 0.45); // 0.25 + 0.2: deterministic step model
        h.record_success(300);
        assert_eq!(h.confidence, 0.65);
    }

    #[test]
    fn confidence_caps_at_one() {
        let mut h = EndpointHealth::new(id("ep-1"));
        for t in 0..10 {
            h.record_success(t);
        }
        assert_eq!(h.confidence, 1.0);
    }

    #[test]
    fn failures_trigger_cooldown_then_expire() {
        let mut h = EndpointHealth::new(id("ep-1"));
        h.record_failure(1_000, 3, 60_000);
        h.record_failure(1_001, 3, 60_000);
        assert!(h.is_healthy(1_002)); // only 2 failures, threshold 3
        h.record_failure(1_002, 3, 60_000);
        assert!(!h.is_healthy(1_003));
        assert_eq!(h.cooldown_until_millis, Some(61_002));
        assert!(h.is_healthy(61_002)); // deadline is exclusive
        assert_eq!(h.confidence, 0.0); // floored
    }

    #[test]
    fn single_failure_threshold_cools_immediately() {
        let mut h = EndpointHealth::new(id("ep-1"));
        h.record_failure(500, 1, 10_000);
        assert!(!h.is_healthy(501));
        assert!(h.is_healthy(10_500));
    }

    #[test]
    fn table_creates_records_on_demand_and_lists_deterministically() {
        let mut table = HealthTable::new();
        assert!(table.get(&id("ep-b")).is_none());
        table.record(id("ep-b")).record_success(10);
        table.record(id("ep-a")).record_failure(10, 1, 60_000);
        let healthy = table.healthy_endpoints(11);
        assert_eq!(healthy, vec![&id("ep-b")]);
    }

    #[test]
    fn health_serializes_for_snapshots() {
        let mut h = EndpointHealth::new(id("ep-1"));
        h.record_success(42);
        let json = serde_json::to_string(&h).unwrap();
        let back: EndpointHealth = serde_json::from_str(&json).unwrap();
        assert_eq!(h, back);
    }
}
