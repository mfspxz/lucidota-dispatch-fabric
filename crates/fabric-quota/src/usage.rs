//! T045: usage recording.
//!
//! Records what the provider *reported* — input/output tokens, request
//! counts, latency, errors, and estimated cost — exactly where available.
//! Every field the provider did not report stays `None`; aggregates sum only
//! what was reported and say so.

use crate::UnixMillis;
use fabric_types::EndpointId;
use serde::{Deserialize, Serialize};

/// One provider-reported usage observation. Fields the provider did not
/// report are `None`, never zero-filled: zero tokens reported is data, a
/// missing token count is unknown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRecord {
    pub endpoint: EndpointId,
    pub at_ms: UnixMillis,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// End-to-end latency in milliseconds, if reported.
    pub latency_ms: Option<u64>,
    /// Machine-readable error kind when the request failed, e.g.
    /// `quota_exhausted`. `None` means the request succeeded.
    pub error: Option<String>,
    /// Estimated cost in USD, if the provider reports pricing data.
    pub estimated_cost_usd: Option<f64>,
}

impl UsageRecord {
    /// A successful request with the given reported figures.
    pub fn success(
        endpoint: EndpointId,
        at_ms: UnixMillis,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        latency_ms: Option<u64>,
        estimated_cost_usd: Option<f64>,
    ) -> Self {
        Self {
            endpoint,
            at_ms,
            input_tokens,
            output_tokens,
            latency_ms,
            error: None,
            estimated_cost_usd,
        }
    }

    /// A failed request. Token/latency figures may still be reported.
    pub fn failure(
        endpoint: EndpointId,
        at_ms: UnixMillis,
        error: impl Into<String>,
        latency_ms: Option<u64>,
    ) -> Self {
        Self {
            endpoint,
            at_ms,
            input_tokens: None,
            output_tokens: None,
            latency_ms,
            error: Some(error.into()),
            estimated_cost_usd: None,
        }
    }

    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

/// Aggregated usage for one endpoint over the recorded observations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageTotals {
    pub requests: u64,
    pub errors: u64,
    /// Sums of *reported* values only.
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub latency_ms_total: u64,
    pub latency_samples: u64,
    /// Sum of reported costs. `None` when no observation reported a cost.
    pub estimated_cost_usd: Option<f64>,
    /// How many observations lacked each figure (unknown, not zero).
    pub missing_token_reports: u64,
    pub missing_cost_reports: u64,
}

impl UsageTotals {
    /// Mean latency over reported samples; `None` when none reported.
    pub fn mean_latency_ms(&self) -> Option<f64> {
        if self.latency_samples == 0 {
            None
        } else {
            Some(self.latency_ms_total as f64 / self.latency_samples as f64)
        }
    }

    pub fn error_rate(&self) -> Option<f64> {
        if self.requests == 0 {
            None
        } else {
            Some(self.errors as f64 / self.requests as f64)
        }
    }
}

/// Append-only log of usage observations with per-endpoint aggregation.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct UsageLog {
    records: Vec<UsageRecord>,
}

impl UsageLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one observation exactly as reported.
    pub fn record(&mut self, record: UsageRecord) {
        self.records.push(record);
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Aggregate all observations for one endpoint.
    pub fn totals(&self, endpoint: &EndpointId) -> UsageTotals {
        let mut t = UsageTotals {
            requests: 0,
            errors: 0,
            input_tokens: 0,
            output_tokens: 0,
            latency_ms_total: 0,
            latency_samples: 0,
            estimated_cost_usd: None,
            missing_token_reports: 0,
            missing_cost_reports: 0,
        };
        for r in self.records.iter().filter(|r| &r.endpoint == endpoint) {
            t.requests += 1;
            if r.is_error() {
                t.errors += 1;
            }
            match (r.input_tokens, r.output_tokens) {
                (Some(i), Some(o)) => {
                    t.input_tokens += i;
                    t.output_tokens += o;
                }
                _ => t.missing_token_reports += 1,
            }
            if let Some(l) = r.latency_ms {
                t.latency_ms_total += l;
                t.latency_samples += 1;
            }
            match r.estimated_cost_usd {
                Some(c) => t.estimated_cost_usd = Some(t.estimated_cost_usd.unwrap_or(0.0) + c),
                None => t.missing_cost_reports += 1,
            }
        }
        t
    }

    /// Iterate observations for one endpoint in insertion order.
    pub fn for_endpoint<'a>(
        &'a self,
        endpoint: &'a EndpointId,
    ) -> impl Iterator<Item = &'a UsageRecord> {
        self.records.iter().filter(move |r| &r.endpoint == endpoint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(name: &str) -> EndpointId {
        EndpointId::new(name).unwrap()
    }

    #[test]
    fn records_and_aggregates_reported_figures() {
        let mut log = UsageLog::new();
        log.record(UsageRecord::success(
            ep("a"),
            1000,
            Some(100),
            Some(50),
            Some(200),
            Some(0.001),
        ));
        log.record(UsageRecord::success(
            ep("a"),
            2000,
            Some(300),
            Some(150),
            Some(400),
            Some(0.003),
        ));
        log.record(UsageRecord::failure(ep("a"), 3000, "timeout", Some(5000)));
        log.record(UsageRecord::success(ep("b"), 4000, Some(10), Some(5), None, None));

        let t = log.totals(&ep("a"));
        assert_eq!(t.requests, 3);
        assert_eq!(t.errors, 1);
        assert_eq!(t.input_tokens, 400);
        assert_eq!(t.output_tokens, 200);
        assert_eq!(t.latency_ms_total, 5600);
        assert_eq!(t.latency_samples, 3);
        assert_eq!(t.estimated_cost_usd, Some(0.004));
        assert_eq!(t.missing_token_reports, 1); // the failure reported no tokens
        assert_eq!(t.missing_cost_reports, 1);
        assert_eq!(t.mean_latency_ms(), Some(5600.0 / 3.0));
        assert!((t.error_rate().unwrap() - 1.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn unknown_stays_unknown_in_aggregates() {
        let mut log = UsageLog::new();
        log.record(UsageRecord::success(ep("a"), 0, None, None, None, None));
        let t = log.totals(&ep("a"));
        assert_eq!(t.requests, 1);
        assert_eq!(t.input_tokens, 0); // nothing reported: sum of nothing
        assert_eq!(t.missing_token_reports, 1);
        assert!(t.estimated_cost_usd.is_none()); // no cost reported anywhere
        assert!(t.mean_latency_ms().is_none());
        assert_eq!(t.error_rate(), Some(0.0));
    }

    #[test]
    fn empty_log_has_empty_totals() {
        let log = UsageLog::new();
        let t = log.totals(&ep("a"));
        assert_eq!(t.requests, 0);
        assert!(t.error_rate().is_none());
    }

    #[test]
    fn endpoints_are_isolated() {
        let mut log = UsageLog::new();
        log.record(UsageRecord::success(ep("a"), 0, Some(1), Some(1), None, None));
        let t = log.totals(&ep("b"));
        assert_eq!(t.requests, 0);
        assert_eq!(log.for_endpoint(&ep("a")).count(), 1);
    }

    #[test]
    fn record_helpers_classify_correctly() {
        let s = UsageRecord::success(ep("a"), 0, None, None, None, None);
        assert!(!s.is_error());
        let f = UsageRecord::failure(ep("a"), 0, "quota_exhausted", None);
        assert!(f.is_error());
        assert_eq!(f.error.as_deref(), Some("quota_exhausted"));
    }
}
