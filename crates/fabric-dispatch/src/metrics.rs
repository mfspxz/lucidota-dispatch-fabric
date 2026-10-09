//! T070: dispatch metrics by task class and endpoint.
//!
//! Exposes queue wait, service time, retries, errors, utilization, and
//! throughput, keyed by (task class, endpoint). All rates are computed over a
//! sliding window so a restart or a quiet period cannot inflate history.
//!
//! - **queue wait**: dispatch_ms - enqueue_ms, recorded at dispatch.
//! - **service time**: completion_ms - dispatch_ms, recorded at completion.
//! - **retries**: attempts beyond the first, summed per key.
//! - **errors**: completions with non-Pass verdicts plus transport/infra errors.
//! - **utilization**: in-flight attempts / capacity (capacity is supplied by
//!   the caller — the registry never invents it; `None` when unknown).
//! - **throughput**: completions inside the window, scaled to per-minute.

use fabric_types::{EndpointId, TaskClass, Verdict};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

/// One aggregated metric point for a (class, endpoint) pair.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricPoint {
    pub class: TaskClass,
    pub endpoint: EndpointId,
    pub dispatched: u64,
    pub completed: u64,
    pub error_count: u64,
    pub total_retries: u64,
    pub avg_wait_ms: Option<f64>,
    pub avg_service_ms: Option<f64>,
    /// Completions per minute over the sliding window.
    pub throughput_per_min: f64,
    pub in_flight: u64,
}

#[derive(Debug, Clone, Default)]
struct KeyStats {
    dispatched: u64,
    completed: u64,
    error_count: u64,
    total_retries: u64,
    wait_ms_total: u64,
    wait_samples: u64,
    service_ms_total: u64,
    service_samples: u64,
    in_flight: u64,
    /// Completion timestamps (unix ms) inside the window, oldest first.
    completions: VecDeque<u64>,
}

/// Metrics registry. `window_ms` bounds throughput memory; inject `now_ms`
/// at every call so tests and replays are deterministic.
#[derive(Debug)]
pub struct MetricsRegistry {
    stats: HashMap<(TaskClass, EndpointId), KeyStats>,
    window_ms: u64,
    max_completion_samples: usize,
}

impl MetricsRegistry {
    pub fn new(window_ms: u64) -> Self {
        Self {
            stats: HashMap::new(),
            window_ms: window_ms.max(1),
            max_completion_samples: 10_000,
        }
    }

    fn key_stats(&mut self, class: TaskClass, endpoint: &EndpointId) -> &mut KeyStats {
        self.stats
            .entry((class, endpoint.clone()))
            .or_default()
    }

    /// Record a dispatch: one attempt left the queue after `wait_ms`.
    pub fn record_dispatch(&mut self, class: TaskClass, endpoint: &EndpointId, wait_ms: u64) {
        let s = self.key_stats(class, endpoint);
        s.dispatched += 1;
        s.in_flight += 1;
        s.wait_ms_total += wait_ms;
        s.wait_samples += 1;
    }

    /// Record a terminal attempt. `retries` = attempts beyond the first for
    /// this task. Non-Pass verdicts count as errors.
    pub fn record_completion(
        &mut self,
        class: TaskClass,
        endpoint: &EndpointId,
        now_ms: u64,
        service_ms: u64,
        verdict: Verdict,
        retries: u32,
    ) {
        let window_ms = self.window_ms;
        let max_samples = self.max_completion_samples;
        let s = self.key_stats(class, endpoint);
        s.completed += 1;
        s.in_flight = s.in_flight.saturating_sub(1);
        s.service_ms_total += service_ms;
        s.service_samples += 1;
        s.total_retries += retries as u64;
        if !verdict.is_pass() {
            s.error_count += 1;
        }
        s.completions.push_back(now_ms);
        while s.completions.len() > max_samples {
            s.completions.pop_front();
        }
        let cutoff = now_ms.saturating_sub(window_ms);
        while s.completions.front().is_some_and(|t| *t < cutoff) {
            s.completions.pop_front();
        }
    }

    /// Record an infrastructure-level error with no attempt completion
    /// (transport failure, timeout before dispatch, ...).
    pub fn record_error(&mut self, class: TaskClass, endpoint: &EndpointId) {
        self.key_stats(class, endpoint).error_count += 1;
    }

    /// Utilization = in-flight / capacity. `None` when capacity is unknown —
    /// never invented.
    pub fn utilization(
        &self,
        class: TaskClass,
        endpoint: &EndpointId,
        capacity: Option<u64>,
    ) -> Option<f64> {
        let capacity = capacity.filter(|c| *c > 0)?;
        let in_flight = self.stats.get(&(class, endpoint.clone()))?.in_flight;
        Some(in_flight as f64 / capacity as f64)
    }

    /// Snapshot of every observed (class, endpoint) pair, sorted for
    /// determinism (class name, then endpoint id).
    pub fn snapshot(&self, now_ms: u64) -> Vec<MetricPoint> {
        let mut points: Vec<MetricPoint> = self
            .stats
            .iter()
            .map(|((class, endpoint), s)| {
                let cutoff = now_ms.saturating_sub(self.window_ms);
                let in_window = s.completions.iter().filter(|t| **t >= cutoff).count();
                MetricPoint {
                    class: class.clone(),
                    endpoint: endpoint.clone(),
                    dispatched: s.dispatched,
                    completed: s.completed,
                    error_count: s.error_count,
                    total_retries: s.total_retries,
                    avg_wait_ms: avg(s.wait_ms_total, s.wait_samples),
                    avg_service_ms: avg(s.service_ms_total, s.service_samples),
                    throughput_per_min: in_window as f64 / (self.window_ms as f64 / 60_000.0),
                    in_flight: s.in_flight,
                }
            })
            .collect();
        points.sort_by(|a, b| {
            a.class
                .to_string()
                .cmp(&b.class.to_string())
                .then(a.endpoint.as_str().cmp(b.endpoint.as_str()))
        });
        points
    }
}

fn avg(total: u64, n: u64) -> Option<f64> {
    if n == 0 {
        None
    } else {
        Some(total as f64 / n as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep() -> EndpointId {
        EndpointId::new("ep-1").unwrap()
    }

    #[test]
    fn dispatch_and_completion_flow() {
        let mut m = MetricsRegistry::new(60_000);
        m.record_dispatch(TaskClass::Implement, &ep(), 500);
        m.record_dispatch(TaskClass::Implement, &ep(), 1500);
        assert_eq!(
            m.utilization(TaskClass::Implement, &ep(), Some(4)),
            Some(0.5)
        );
        m.record_completion(TaskClass::Implement, &ep(), 61_000, 2_000, Verdict::Pass, 1);
        let pts = m.snapshot(61_000);
        assert_eq!(pts.len(), 1);
        let p = &pts[0];
        assert_eq!(p.dispatched, 2);
        assert_eq!(p.completed, 1);
        assert_eq!(p.error_count, 0);
        assert_eq!(p.total_retries, 1);
        assert_eq!(p.avg_wait_ms, Some(1000.0));
        assert_eq!(p.avg_service_ms, Some(2000.0));
        assert_eq!(p.in_flight, 1);
        assert!((p.throughput_per_min - 1.0).abs() < 1e-9);
    }

    #[test]
    fn non_pass_verdicts_count_as_errors() {
        let mut m = MetricsRegistry::new(60_000);
        m.record_dispatch(TaskClass::Verify, &ep(), 10);
        m.record_completion(TaskClass::Verify, &ep(), 1000, 100, Verdict::Fail, 0);
        m.record_error(TaskClass::Verify, &ep());
        let p = &m.snapshot(1000)[0];
        assert_eq!(p.error_count, 2);
        assert_eq!(p.completed, 1);
    }

    #[test]
    fn utilization_unknown_capacity_is_none() {
        let m = MetricsRegistry::new(60_000);
        assert_eq!(m.utilization(TaskClass::Plan, &ep(), None), None);
        assert_eq!(m.utilization(TaskClass::Plan, &ep(), Some(0)), None);
        assert_eq!(m.utilization(TaskClass::Plan, &ep(), Some(8)), None); // never observed
    }

    #[test]
    fn throughput_window_slides() {
        let mut m = MetricsRegistry::new(60_000);
        m.record_dispatch(TaskClass::Review, &ep(), 0);
        m.record_completion(TaskClass::Review, &ep(), 1_000, 10, Verdict::Pass, 0);
        // Snapshot far in the future: the completion aged out of the window.
        let p = &m.snapshot(1_000_000)[0];
        assert_eq!(p.throughput_per_min, 0.0);
        assert_eq!(p.completed, 1); // cumulative counters are not windowed
    }

    #[test]
    fn snapshot_is_deterministic_across_keys() {
        let mut m = MetricsRegistry::new(60_000);
        let ep2 = EndpointId::new("ep-2").unwrap();
        m.record_dispatch(TaskClass::Verify, &ep2, 1);
        m.record_dispatch(TaskClass::Implement, &ep(), 1);
        let pts = m.snapshot(0);
        assert_eq!(pts[0].class, TaskClass::Implement);
        assert_eq!(pts[1].class, TaskClass::Verify);
    }

    #[test]
    fn empty_registry_snapshots_empty() {
        let m = MetricsRegistry::new(60_000);
        assert!(m.snapshot(0).is_empty());
    }
}
