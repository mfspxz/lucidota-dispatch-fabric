//! T025: rolling latency and completion statistics.
//!
//! Bounded per-(endpoint, task class) sample windows. Reports mean latency,
//! nearest-rank p95 latency, and completion rate over the current window.
//! Windows are fixed-capacity ring buffers: old samples age out, memory is
//! bounded, and every aggregate is computed from the same window.

use fabric_types::{EndpointId, TaskClass};
use std::collections::{HashMap, VecDeque};

/// Bounded rolling window of latency samples for one (endpoint, class) pair.
#[derive(Debug, Clone)]
pub struct RollingStats {
    samples: VecDeque<f64>,
    capacity: usize,
    attempts: u64,
    errors: u64,
}

impl RollingStats {
    /// Create a window holding at most `capacity` samples (minimum 1).
    pub fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity.max(1)),
            capacity: capacity.max(1),
            attempts: 0,
            errors: 0,
        }
    }

    /// Record one attempt: its latency in milliseconds and whether it
    /// completed without error. Oldest sample evicted when full.
    pub fn record(&mut self, latency_ms: f64, completed_ok: bool) {
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(latency_ms.max(0.0));
        self.attempts += 1;
        if !completed_ok {
            self.errors += 1;
        }
    }

    /// Samples currently in the window.
    pub fn count(&self) -> usize {
        self.samples.len()
    }

    /// Total attempts ever recorded (including evicted).
    pub fn attempts(&self) -> u64 {
        self.attempts
    }

    /// Mean latency over the window, or None when empty.
    pub fn mean(&self) -> Option<f64> {
        if self.samples.is_empty() {
            return None;
        }
        Some(self.samples.iter().sum::<f64>() / self.samples.len() as f64)
    }

    /// Nearest-rank p95 latency over the window, or None when empty.
    pub fn p95(&self) -> Option<f64> {
        if self.samples.is_empty() {
            return None;
        }
        let mut sorted: Vec<f64> = self.samples.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let rank = ((0.95 * sorted.len() as f64).ceil() as usize).max(1) - 1;
        Some(sorted[rank])
    }

    /// Fraction of attempts that completed without error, or None when no
    /// attempts have been recorded.
    pub fn completion_rate(&self) -> Option<f64> {
        if self.attempts == 0 {
            return None;
        }
        Some(1.0 - self.errors as f64 / self.attempts as f64)
    }
}

/// Statistics table keyed by (endpoint, task class).
#[derive(Debug)]
pub struct StatsTable {
    window: usize,
    table: HashMap<(EndpointId, TaskClass), RollingStats>,
}

impl StatsTable {
    /// Create a table whose per-pair windows hold at most `window` samples.
    pub fn new(window: usize) -> Self {
        Self {
            window: window.max(1),
            table: HashMap::new(),
        }
    }

    /// Record one attempt for an endpoint/class pair.
    pub fn record(
        &mut self,
        endpoint: EndpointId,
        class: TaskClass,
        latency_ms: f64,
        completed_ok: bool,
    ) {
        let window = self.window;
        self.table
            .entry((endpoint, class))
            .or_insert_with(|| RollingStats::new(window))
            .record(latency_ms, completed_ok);
    }

    /// Borrow the stats for a pair, if any samples exist.
    pub fn get(
        &self,
        endpoint: &EndpointId,
        class: &TaskClass,
    ) -> Option<&RollingStats> {
        self.table.get(&(endpoint.clone(), class.clone()))
    }

    /// All pairs with samples, in deterministic (endpoint, class-name) order.
    pub fn pairs(&self) -> Vec<(&EndpointId, &TaskClass)> {
        let mut out: Vec<(&EndpointId, &TaskClass)> = self
            .table
            .keys()
            .map(|(e, c)| (e, c))
            .collect();
        out.sort_by(|a, b| a.0.cmp(b.0).then(a.1.name().cmp(b.1.name())));
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
    fn empty_window_has_no_aggregates() {
        let s = RollingStats::new(10);
        assert_eq!(s.count(), 0);
        assert_eq!(s.mean(), None);
        assert_eq!(s.p95(), None);
        assert_eq!(s.completion_rate(), None);
    }

    #[test]
    fn window_evicts_oldest_samples() {
        let mut s = RollingStats::new(3);
        for v in [10.0, 20.0, 30.0, 40.0] {
            s.record(v, true);
        }
        assert_eq!(s.count(), 3);
        assert_eq!(s.attempts(), 4);
        assert_eq!(s.mean(), Some(30.0)); // 20+30+40
    }

    #[test]
    fn p95_nearest_rank() {
        let mut s = RollingStats::new(20);
        for v in 1..=20 {
            s.record(v as f64, true);
        }
        // ceil(0.95*20) = 19 -> 19th of 1..=20
        assert_eq!(s.p95(), Some(19.0));
    }

    #[test]
    fn completion_rate_counts_errors() {
        let mut s = RollingStats::new(10);
        s.record(5.0, true);
        s.record(5.0, false);
        s.record(5.0, true);
        s.record(5.0, false);
        assert_eq!(s.completion_rate(), Some(0.5));
    }

    #[test]
    fn negative_latency_clamps_to_zero() {
        let mut s = RollingStats::new(4);
        s.record(-3.0, true);
        assert_eq!(s.mean(), Some(0.0));
    }

    #[test]
    fn table_partitions_by_endpoint_and_class() {
        let mut t = StatsTable::new(8);
        t.record(id("ep-a"), TaskClass::Implement, 10.0, true);
        t.record(id("ep-a"), TaskClass::Review, 50.0, false);
        t.record(id("ep-b"), TaskClass::Implement, 12.0, true);
        assert_eq!(t.get(&id("ep-a"), &TaskClass::Implement).unwrap().mean(), Some(10.0));
        assert_eq!(
            t.get(&id("ep-a"), &TaskClass::Review).unwrap().completion_rate(),
            Some(0.0)
        );
        assert!(t.get(&id("ep-b"), &TaskClass::Review).is_none());
        let pairs = t.pairs();
        assert_eq!(pairs.len(), 3);
        // deterministic order: ep-a/IMPLEMENT, ep-a/REVIEW, ep-b/IMPLEMENT
        assert_eq!(pairs[0], (&id("ep-a"), &TaskClass::Implement));
    }
}
