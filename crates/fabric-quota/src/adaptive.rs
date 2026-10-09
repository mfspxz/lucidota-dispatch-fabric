//! T049: adaptive dispatch throttling.
//!
//! AIMD (additive-increase / multiplicative-decrease) rate control per
//! endpoint: quota errors cut the rate multiplicatively, sustained high
//! latency cuts it gently, and consecutive successful windows raise it
//! additively back toward the ceiling. Recovery is gradual by design —
//! never a jump straight back to max. All clocks are injected.

use crate::UnixMillis;
use fabric_types::{FabricError, Result};
use serde::{Deserialize, Serialize};

/// Tuning for one adaptive throttle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdaptiveConfig {
    /// Floor: dispatches per second. Must satisfy 0 < min <= max.
    pub min_rate_per_sec: f64,
    /// Ceiling: dispatches per second.
    pub max_rate_per_sec: f64,
    /// Multiplicative decrease on quota error; must satisfy 0 < f < 1.
    pub decrease_factor: f64,
    /// Additive increase per successful window; must be positive.
    pub increase_step_per_sec: f64,
    /// Consecutive successes required before one increase step; >= 1.
    pub successes_per_window: u32,
    /// Latency samples above this (ms) count as congestion; positive.
    pub latency_threshold_ms: f64,
}

impl AdaptiveConfig {
    pub fn validate(&self) -> Result<()> {
        if self.min_rate_per_sec <= 0.0
            || !self.min_rate_per_sec.is_finite()
            || self.max_rate_per_sec <= 0.0
            || !self.max_rate_per_sec.is_finite()
            || self.min_rate_per_sec > self.max_rate_per_sec
        {
            return Err(FabricError::InvalidInput {
                field: "adaptive.min/max_rate_per_sec",
                reason: "require 0 < min <= max, both finite".to_string(),
            });
        }
        if self.decrease_factor <= 0.0
            || self.decrease_factor >= 1.0
            || !self.decrease_factor.is_finite()
        {
            return Err(FabricError::InvalidInput {
                field: "adaptive.decrease_factor",
                reason: "require 0 < factor < 1".to_string(),
            });
        }
        if self.increase_step_per_sec <= 0.0 || !self.increase_step_per_sec.is_finite() {
            return Err(FabricError::InvalidInput {
                field: "adaptive.increase_step_per_sec",
                reason: "must be finite and positive".to_string(),
            });
        }
        if self.successes_per_window == 0 {
            return Err(FabricError::InvalidInput {
                field: "adaptive.successes_per_window",
                reason: "must be >= 1".to_string(),
            });
        }
        if self.latency_threshold_ms <= 0.0 || !self.latency_threshold_ms.is_finite() {
            return Err(FabricError::InvalidInput {
                field: "adaptive.latency_threshold_ms",
                reason: "must be finite and positive".to_string(),
            });
        }
        Ok(())
    }
}

/// Per-endpoint adaptive throttle. Starts at the configured ceiling.
#[derive(Debug)]
pub struct AdaptiveThrottle {
    config: AdaptiveConfig,
    current_rate: f64,
    tokens: f64,
    last_ms: UnixMillis,
    consecutive_successes: u32,
}

impl AdaptiveThrottle {
    pub fn new(config: AdaptiveConfig, now: UnixMillis) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            current_rate: config.max_rate_per_sec,
            tokens: config.max_rate_per_sec,
            last_ms: now,
            consecutive_successes: 0,
            config,
        })
    }

    fn refill(&mut self, now: UnixMillis) {
        let dt_secs = now.saturating_sub(self.last_ms) as f64 / 1000.0;
        self.tokens = (self.tokens + dt_secs * self.current_rate)
            .min(self.current_rate.max(1.0));
        self.last_ms = now;
    }

    /// Gate one dispatch at `now`. Returns true and consumes a token when the
    /// current rate allows it.
    pub fn allow(&mut self, now: UnixMillis) -> bool {
        self.refill(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// A dispatch hit a quota error: multiplicative decrease, floored at min.
    /// The success streak resets — recovery must be re-earned.
    pub fn on_quota_error(&mut self) {
        self.current_rate = (self.current_rate * self.config.decrease_factor)
            .max(self.config.min_rate_per_sec);
        self.tokens = self.tokens.min(self.current_rate.max(1.0));
        self.consecutive_successes = 0;
    }

    /// A latency sample. Sustained samples above the threshold are congestion:
    /// decrease gently (halfway between a quota-error cut and holding steady).
    pub fn on_latency_sample(&mut self, latency_ms: f64) {
        if latency_ms > self.config.latency_threshold_ms {
            let gentle = (1.0 + self.config.decrease_factor) / 2.0;
            self.current_rate =
                (self.current_rate * gentle).max(self.config.min_rate_per_sec);
            self.consecutive_successes = 0;
        }
    }

    /// A dispatch succeeded. After `successes_per_window` consecutive
    /// successes the rate rises one additive step toward the ceiling.
    pub fn on_success(&mut self) {
        self.consecutive_successes = self.consecutive_successes.saturating_add(1);
        if self.consecutive_successes >= self.config.successes_per_window {
            self.consecutive_successes = 0;
            self.current_rate = (self.current_rate + self.config.increase_step_per_sec)
                .min(self.config.max_rate_per_sec);
        }
    }

    pub fn current_rate(&self) -> f64 {
        self.current_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> AdaptiveConfig {
        AdaptiveConfig {
            min_rate_per_sec: 1.0,
            max_rate_per_sec: 10.0,
            decrease_factor: 0.5,
            increase_step_per_sec: 2.0,
            successes_per_window: 3,
            latency_threshold_ms: 2000.0,
        }
    }

    #[test]
    fn quota_errors_decrease_multiplicatively_to_floor() {
        let mut t = AdaptiveThrottle::new(config(), 0).unwrap();
        t.on_quota_error();
        assert_eq!(t.current_rate(), 5.0);
        t.on_quota_error();
        assert_eq!(t.current_rate(), 2.5);
        for _ in 0..10 {
            t.on_quota_error();
        }
        assert_eq!(t.current_rate(), 1.0, "floored at min");
    }

    #[test]
    fn recovery_is_gradual_not_instant() {
        let mut t = AdaptiveThrottle::new(config(), 0).unwrap();
        t.on_quota_error(); // 10 -> 5
        // one success is not enough (window = 3)
        t.on_success();
        assert_eq!(t.current_rate(), 5.0);
        t.on_success();
        t.on_success(); // window complete -> +2
        assert_eq!(t.current_rate(), 7.0);
        // quota error resets the streak
        t.on_quota_error(); // 7 -> 3.5
        t.on_success();
        t.on_success();
        assert_eq!(t.current_rate(), 3.5, "streak reset, no increase yet");
    }

    #[test]
    fn recovery_never_exceeds_ceiling() {
        let mut t = AdaptiveThrottle::new(config(), 0).unwrap();
        for _ in 0..30 {
            t.on_success();
        }
        assert_eq!(t.current_rate(), 10.0);
    }

    #[test]
    fn rising_latency_decreases_gently() {
        let mut t = AdaptiveThrottle::new(config(), 0).unwrap();
        t.on_latency_sample(1500.0); // below threshold: no change
        assert_eq!(t.current_rate(), 10.0);
        t.on_latency_sample(5000.0); // above: gentle cut (factor 0.75)
        assert_eq!(t.current_rate(), 7.5);
    }

    #[test]
    fn allow_gates_on_current_rate() {
        let mut t = AdaptiveThrottle::new(config(), 0).unwrap();
        // 10 tokens at t=0, rate 10/s: 10 immediate allows, then gated
        let mut allowed = 0;
        for _ in 0..15 {
            if t.allow(0) {
                allowed += 1;
            }
        }
        assert_eq!(allowed, 10);
        // after 1s, 10 more tokens
        assert!(t.allow(1_000));
        // after a quota error the rate is 5/s: fewer tokens accumulate
        t.on_quota_error();
        let mut allowed2 = 0;
        for _ in 0..10 {
            if t.allow(1_000) {
                allowed2 += 1;
            }
        }
        assert!(allowed2 <= 6, "throttled rate admits fewer, got {allowed2}");
    }

    #[test]
    fn invalid_configs_rejected() {
        let mut c = config();
        c.min_rate_per_sec = 0.0;
        assert!(AdaptiveThrottle::new(c, 0).is_err());
        let mut c = config();
        c.min_rate_per_sec = 20.0;
        assert!(AdaptiveThrottle::new(c, 0).is_err());
        let mut c = config();
        c.decrease_factor = 1.0;
        assert!(AdaptiveThrottle::new(c, 0).is_err());
        let mut c = config();
        c.successes_per_window = 0;
        assert!(AdaptiveThrottle::new(c, 0).is_err());
    }
}
