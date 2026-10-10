//! T099 — verification result cache with exact-match keys.
//!
//! A cache hit requires ALL FIVE key components to match: source digest,
//! command, environment fingerprint, verifier version, and input digests.
//! Any mismatch is a miss, never a hit. Entries carry their full key so a
//! mismatch is provable. Eviction is deterministic LRU with a fixed cap:
//! the least-recently-used entry is evicted first, ties broken by
//! insertion order.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The full cache key. Every field participates in equality; a `None`
/// in one entry never equals a `Some` in another.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CacheKey {
    pub source_digest: String,
    pub command: Vec<String>,
    pub env_fingerprint: String,
    pub verifier_version: String,
    pub input_digests: Vec<String>,
}

impl CacheKey {
    /// A cache entry is only usable against a key with all five components
    /// non-empty. Anything else is unusable for hits (constructible but
    /// never matched).
    pub fn usable(&self) -> bool {
        !self.source_digest.is_empty()
            && !self.command.is_empty()
            && !self.env_fingerprint.is_empty()
            && !self.verifier_version.is_empty()
            && !self.input_digests.is_empty()
    }
}

/// Cached verification result, carrying its full key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheEntry {
    pub key: CacheKey,
    /// Whether the verification passed, encoded without ambiguity.
    pub passed: bool,
    /// Digest of the normalized verifier output (evidence reference).
    pub output_digest: String,
}

/// Deterministic LRU verification cache with a fixed capacity.
/// Empty capacity is rejected at construction.
#[derive(Debug)]
pub struct VerificationCache {
    capacity: usize,
    entries: HashMap<CacheKey, (CacheEntry, u64)>,
    clock: u64,
}

impl VerificationCache {
    pub fn with_capacity(capacity: usize) -> Option<Self> {
        if capacity == 0 {
            return None;
        }
        Some(VerificationCache {
            capacity,
            entries: HashMap::new(),
            clock: 0,
        })
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Insert a result. Any existing entry for an equal key is replaced.
    /// Beyond capacity, the least-recently-used entry is evicted.
    pub fn insert(&mut self, entry: CacheEntry) {
        self.clock += 1;
        let now = self.clock;
        self.entries.insert(entry.key.clone(), (entry, now));
        while self.entries.len() > self.capacity {
            let lru_key = self
                .entries
                .iter()
                .min_by(|a, b| {
                    a.1
                         .1
                        .cmp(&b.1 .1)
                        .then_with(|| a.0.source_digest.cmp(&b.0.source_digest))
                })
                .map(|(k, _)| k.clone());
            if let Some(k) = lru_key {
                self.entries.remove(&k);
            } else {
                break;
            }
        }
    }

    /// Look up a result. A hit requires exact equality of all five key
    /// components; any mismatch is a miss. A hit refreshes recency.
    pub fn get(&mut self, key: &CacheKey) -> Option<CacheEntry> {
        if !key.usable() {
            return None;
        }
        self.clock += 1;
        let now = self.clock;
        self.entries.get_mut(key).map(|(entry, stamp)| {
            *stamp = now;
            entry.clone()
        })
    }

    /// Explicit invalidation: remove every entry for a source digest.
    /// Returns the number removed.
    pub fn invalidate_source(&mut self, source_digest: &str) -> usize {
        let before = self.entries.len();
        self.entries.retain(|k, _| k.source_digest != source_digest);
        before - self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(src: &str, ver: &str) -> CacheKey {
        CacheKey {
            source_digest: src.to_string(),
            command: vec!["cargo".to_string(), "test".to_string()],
            env_fingerprint: "env:linux-x86_64".to_string(),
            verifier_version: ver.to_string(),
            input_digests: vec!["sha256:in1".to_string()],
        }
    }

    fn entry(src: &str, ver: &str, passed: bool) -> CacheEntry {
        CacheEntry {
            key: key(src, ver),
            passed,
            output_digest: "sha256:out".to_string(),
        }
    }

    #[test]
    fn exact_match_is_a_hit() {
        let mut c = VerificationCache::with_capacity(4).unwrap();
        c.insert(entry("sha256:s1", "v1", true));
        let hit = c.get(&key("sha256:s1", "v1"));
        assert_eq!(hit.map(|e| e.passed), Some(true));
    }

    #[test]
    fn any_single_mismatch_is_a_miss() {
        let mut c = VerificationCache::with_capacity(8).unwrap();
        c.insert(entry("sha256:s1", "v1", true));
        let mut k = key("sha256:s1", "v1");
        k.source_digest = "sha256:s2".to_string();
        assert!(c.get(&k).is_none());
        k = key("sha256:s1", "v1");
        k.command = vec!["other".to_string()];
        assert!(c.get(&k).is_none());
        k = key("sha256:s1", "v1");
        k.env_fingerprint = "env:other".to_string();
        assert!(c.get(&k).is_none());
        k = key("sha256:s1", "v1");
        k.verifier_version = "v2".to_string();
        assert!(c.get(&k).is_none());
        k = key("sha256:s1", "v1");
        k.input_digests = vec!["sha256:other".to_string()];
        assert!(c.get(&k).is_none());
    }

    #[test]
    fn unusable_key_is_never_a_hit() {
        let mut c = VerificationCache::with_capacity(4).unwrap();
        c.insert(entry("sha256:s1", "v1", true));
        let mut k = key("sha256:s1", "v1");
        k.input_digests.clear();
        assert!(c.get(&k).is_none());
    }

    #[test]
    fn lru_eviction_is_deterministic() {
        let mut c = VerificationCache::with_capacity(2).unwrap();
        c.insert(entry("sha256:a", "v1", true));
        c.insert(entry("sha256:b", "v1", true));
        // touch a so b is LRU
        let _ = c.get(&key("sha256:a", "v1"));
        c.insert(entry("sha256:c", "v1", true));
        assert_eq!(c.len(), 2);
        assert!(c.get(&key("sha256:b", "v1")).is_none(), "LRU entry evicted");
        assert!(c.get(&key("sha256:a", "v1")).is_some());
        assert!(c.get(&key("sha256:c", "v1")).is_some());
    }

    #[test]
    fn zero_capacity_is_rejected() {
        assert!(VerificationCache::with_capacity(0).is_none());
    }

    #[test]
    fn invalidate_source_removes_only_matching() {
        let mut c = VerificationCache::with_capacity(8).unwrap();
        c.insert(entry("sha256:a", "v1", true));
        c.insert(entry("sha256:a", "v2", false));
        c.insert(entry("sha256:b", "v1", true));
        assert_eq!(c.invalidate_source("sha256:a"), 2);
        assert!(c.get(&key("sha256:b", "v1")).is_some());
    }
}
