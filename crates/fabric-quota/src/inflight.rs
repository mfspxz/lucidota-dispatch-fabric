//! T044: endpoint-specific and global in-flight request limits.
//!
//! Tracks how many requests are currently executing per endpoint and
//! globally. `try_start` refuses to exceed either cap; `finish` releases the
//! slot. Limits are explicit: an endpoint with no configured cap is
//! unconstrained on that dimension (the cap is unknown, not zero), and the
//! global cap is `Option<u64>` for the same reason.

use fabric_types::EndpointId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Why a request was refused a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DenyReason {
    /// The global in-flight cap is reached.
    Global { limit: u64 },
    /// The endpoint's own in-flight cap is reached.
    Endpoint { limit: u64 },
}

/// In-flight request tracker.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct InFlightTracker {
    /// Global cap across all endpoints; `None` = unknown, not enforced.
    global_max: Option<u64>,
    /// Per-endpoint caps; endpoints absent from the map are unconstrained.
    endpoint_max: HashMap<EndpointId, u64>,
    in_flight: HashMap<EndpointId, u64>,
    global_in_flight: u64,
}

impl InFlightTracker {
    /// Create a tracker. `global_max = None` means the global limit is
    /// unknown and therefore not enforced (never defaulted to a guess).
    pub fn new(global_max: Option<u64>) -> Self {
        Self {
            global_max,
            ..Self::default()
        }
    }

    /// Set (or replace) the in-flight cap for one endpoint.
    pub fn set_endpoint_limit(&mut self, endpoint: EndpointId, max: u64) {
        self.endpoint_max.insert(endpoint, max);
    }

    /// Remove an endpoint's cap, returning it to unconstrained.
    pub fn clear_endpoint_limit(&mut self, endpoint: &EndpointId) -> Option<u64> {
        self.endpoint_max.remove(endpoint)
    }

    /// Attempt to start a request on `endpoint`.
    ///
    /// Checks the global cap first (a global breach is the more informative
    /// denial), then the endpoint cap. On success the slot is held until
    /// [`InFlightTracker::finish`].
    pub fn try_start(&mut self, endpoint: &EndpointId) -> Result<(), DenyReason> {
        if let Some(max) = self.global_max {
            if self.global_in_flight >= max {
                return Err(DenyReason::Global { limit: max });
            }
        }
        if let Some(max) = self.endpoint_max.get(endpoint) {
            let current = self.in_flight.get(endpoint).copied().unwrap_or(0);
            if current >= *max {
                return Err(DenyReason::Endpoint { limit: *max });
            }
        }
        *self.in_flight.entry(endpoint.clone()).or_insert(0) += 1;
        self.global_in_flight += 1;
        Ok(())
    }

    /// Release a slot previously taken by [`InFlightTracker::try_start`].
    /// Saturates at zero: releasing more than was taken is a caller bug, not
    /// a negative count.
    pub fn finish(&mut self, endpoint: &EndpointId) {
        if let Some(count) = self.in_flight.get_mut(endpoint) {
            if *count > 0 {
                *count -= 1;
                self.global_in_flight = self.global_in_flight.saturating_sub(1);
            }
            if *count == 0 {
                self.in_flight.remove(endpoint);
            }
        }
    }

    /// Current in-flight count for one endpoint.
    pub fn in_flight(&self, endpoint: &EndpointId) -> u64 {
        self.in_flight.get(endpoint).copied().unwrap_or(0)
    }

    /// Current global in-flight count.
    pub fn global_in_flight(&self) -> u64 {
        self.global_in_flight
    }

    /// Configured cap for one endpoint, if any.
    pub fn endpoint_limit(&self, endpoint: &EndpointId) -> Option<u64> {
        self.endpoint_max.get(endpoint).copied()
    }

    pub fn global_limit(&self) -> Option<u64> {
        self.global_max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(name: &str) -> EndpointId {
        EndpointId::new(name).unwrap()
    }

    #[test]
    fn enforces_endpoint_cap() {
        let mut t = InFlightTracker::new(None);
        t.set_endpoint_limit(ep("a"), 2);
        assert!(t.try_start(&ep("a")).is_ok());
        assert!(t.try_start(&ep("a")).is_ok());
        assert_eq!(
            t.try_start(&ep("a")),
            Err(DenyReason::Endpoint { limit: 2 })
        );
        assert_eq!(t.in_flight(&ep("a")), 2);
        t.finish(&ep("a"));
        assert!(t.try_start(&ep("a")).is_ok());
    }

    #[test]
    fn enforces_global_cap_across_endpoints() {
        let mut t = InFlightTracker::new(Some(2));
        assert!(t.try_start(&ep("a")).is_ok());
        assert!(t.try_start(&ep("b")).is_ok());
        assert_eq!(t.try_start(&ep("c")), Err(DenyReason::Global { limit: 2 }));
        // Global is checked first even when the endpoint also has a cap.
        t.finish(&ep("a"));
        assert!(t.try_start(&ep("c")).is_ok());
        assert_eq!(t.global_in_flight(), 2);
    }

    #[test]
    fn unknown_limits_are_not_enforced() {
        let mut t = InFlightTracker::new(None); // global unknown
        for _ in 0..1000 {
            assert!(t.try_start(&ep("uncapped")).is_ok());
        }
        assert_eq!(t.global_in_flight(), 1000);
        assert_eq!(t.in_flight(&ep("uncapped")), 1000);
    }

    #[test]
    fn finish_saturates_and_cleans_up() {
        let mut t = InFlightTracker::new(Some(10));
        t.finish(&ep("never-started")); // no panic, no negative
        assert_eq!(t.global_in_flight(), 0);
        t.try_start(&ep("a")).unwrap();
        t.finish(&ep("a"));
        t.finish(&ep("a")); // double release saturates
        assert_eq!(t.global_in_flight(), 0);
        assert_eq!(t.in_flight(&ep("a")), 0);
    }

    #[test]
    fn zero_cap_denies_immediately() {
        let mut t = InFlightTracker::new(None);
        t.set_endpoint_limit(ep("a"), 0);
        assert_eq!(t.try_start(&ep("a")), Err(DenyReason::Endpoint { limit: 0 }));
    }

    #[test]
    fn limit_replacement_and_clear() {
        let mut t = InFlightTracker::new(None);
        t.set_endpoint_limit(ep("a"), 1);
        t.set_endpoint_limit(ep("a"), 3);
        assert_eq!(t.endpoint_limit(&ep("a")), Some(3));
        t.clear_endpoint_limit(&ep("a"));
        assert_eq!(t.endpoint_limit(&ep("a")), None);
        for _ in 0..10 {
            assert!(t.try_start(&ep("a")).is_ok());
        }
    }
}
