//! T068: recover orphaned RUNNING tasks after restart.
//!
//! When the dispatcher restarts, attempts recorded as RUNNING may have lost
//! their worker. Recovery uses **leases**: every RUNNING attempt holds a
//! lease (`lease_holder`, `lease_expires_ms`). A lease that has expired at
//! recovery time means no live worker owns the attempt anymore, so the
//! attempt is cancelled and the task is requeued as attempt N+1. A live lease
//! means hands off.
//!
//! # INTEGRATOR NOTE — durable state store
//!
//! The durable attempt store arrives in a later batch. This module defines
//! the minimal [`DurableState`] trait it needs — `get_attempt`, `put_attempt`,
//! `compare_and_swap`, `running_attempts` — and recovery is written against
//! the trait only. Wire the real store (fabric-ledger or equivalent) to this
//! trait; do not reimplement recovery. The trait's CAS contract:
//! - `compare_and_swap(record, expected_version)` stores `record` **only if**
//!   the stored record's version equals `expected_version`.
//! - `record.version` must equal `expected_version + 1`.
//! - Returns `Ok(true)` on swap, `Ok(false)` on version mismatch (a racing
//!   recovery or the worker itself settled the attempt first).

use crate::progress::AttemptKey;
use fabric_types::{Result, TaskId, TaskState};
use serde::{Deserialize, Serialize};

/// Durable record of one attempt. (See module docs: the store implementing
/// [`DurableState`] arrives in a later batch.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub task_id: TaskId,
    pub attempt: u32,
    pub state: TaskState,
    /// Worker (or recovery) holding the lease.
    pub lease_holder: String,
    /// Unix millis at which the lease expires.
    pub lease_expires_ms: u64,
    /// Monotonic version for compare-and-swap.
    pub version: u64,
}

impl AttemptRecord {
    pub fn key(&self) -> AttemptKey {
        // attempt >= 1 is an invariant of the record; validated at write time.
        AttemptKey {
            task_id: self.task_id.clone(),
            attempt: self.attempt,
        }
    }

    /// True when no live worker can own this attempt anymore.
    pub fn lease_expired(&self, now_ms: u64) -> bool {
        self.lease_expires_ms <= now_ms
    }
}

/// Minimal durable-state interface for attempt records.
/// INTEGRATOR: implement this against the real store when it lands.
pub trait DurableState {
    /// Fetch one attempt record, if present.
    fn get_attempt(&self, task_id: &TaskId, attempt: u32) -> Option<AttemptRecord>;
    /// Blind write of a record (used for brand-new attempts).
    fn put_attempt(&mut self, record: &AttemptRecord) -> Result<()>;
    /// Atomic compare-and-swap: store `record` only if the stored version
    /// equals `expected_version`. `record.version` must be `expected_version + 1`.
    fn compare_and_swap(&mut self, record: &AttemptRecord, expected_version: u64)
        -> Result<bool>;
    /// Every record currently in RUNNING state, for restart scans.
    fn running_attempts(&self) -> Vec<AttemptRecord>;
}

/// What recovery decided for one RUNNING record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Lease expired: old attempt cancelled, task requeued as `to_attempt`.
    Requeued {
        task_id: TaskId,
        from_attempt: u32,
        to_attempt: u32,
    },
    /// Lease still valid: left alone.
    LeaseLive { task_id: TaskId, attempt: u32 },
    /// Record was settled by a racing worker/recovery between scan and CAS.
    AlreadySettled { task_id: TaskId, attempt: u32 },
}

/// Scan RUNNING attempts after restart and recover orphans.
///
/// For each RUNNING record with an expired lease:
/// 1. CAS the record RUNNING -> Cancelled (version+1, holder "recovery").
///    Cancellation here means "this attempt will never produce a verdict",
///    which is exactly what an expired lease guarantees.
/// 2. Write a fresh record for attempt N+1 in Ready state with no lease
///    (`lease_holder` empty, `lease_expires_ms` 0); dispatch will lease it.
///
/// If the CAS loses a race, the attempt settled without us: `AlreadySettled`.
pub fn recover_orphans(
    store: &mut dyn DurableState,
    now_ms: u64,
) -> Result<Vec<RecoveryAction>> {
    let mut actions = Vec::new();
    for record in store.running_attempts() {
        if !record.lease_expired(now_ms) {
            actions.push(RecoveryAction::LeaseLive {
                task_id: record.task_id.clone(),
                attempt: record.attempt,
            });
            continue;
        }
        let cancelled = AttemptRecord {
            state: TaskState::Cancelled,
            lease_holder: "recovery".to_string(),
            version: record.version + 1,
            ..record.clone()
        };
        if !store.compare_and_swap(&cancelled, record.version)? {
            actions.push(RecoveryAction::AlreadySettled {
                task_id: record.task_id.clone(),
                attempt: record.attempt,
            });
            continue;
        }
        let next = AttemptRecord {
            task_id: record.task_id.clone(),
            attempt: record.attempt + 1,
            state: TaskState::Ready,
            lease_holder: String::new(),
            lease_expires_ms: 0,
            version: 0,
        };
        store.put_attempt(&next)?;
        actions.push(RecoveryAction::Requeued {
            task_id: record.task_id.clone(),
            from_attempt: record.attempt,
            to_attempt: next.attempt,
        });
    }
    Ok(actions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// In-memory DurableState for tests. NOT the production store —
    /// the real store arrives in a later batch (see module docs).
    #[derive(Default)]
    struct MemStore {
        map: HashMap<(TaskId, u32), AttemptRecord>,
    }

    impl DurableState for MemStore {
        fn get_attempt(&self, task_id: &TaskId, attempt: u32) -> Option<AttemptRecord> {
            self.map.get(&(task_id.clone(), attempt)).cloned()
        }
        fn put_attempt(&mut self, record: &AttemptRecord) -> Result<()> {
            self.map.insert((record.task_id.clone(), record.attempt), record.clone());
            Ok(())
        }
        fn compare_and_swap(
            &mut self,
            record: &AttemptRecord,
            expected_version: u64,
        ) -> Result<bool> {
            let key = (record.task_id.clone(), record.attempt);
            match self.map.get(&key) {
                Some(cur) if cur.version == expected_version && record.version == expected_version + 1 => {
                    self.map.insert(key, record.clone());
                    Ok(true)
                }
                _ => Ok(false),
            }
        }
        fn running_attempts(&self) -> Vec<AttemptRecord> {
            self.map
                .values()
                .filter(|r| r.state == TaskState::Running)
                .cloned()
                .collect()
        }
    }

    fn running(id: &str, attempt: u32, lease_expires_ms: u64) -> AttemptRecord {
        AttemptRecord {
            task_id: TaskId::new(id).unwrap(),
            attempt,
            state: TaskState::Running,
            lease_holder: "worker-7".to_string(),
            lease_expires_ms,
            version: 3,
        }
    }

    #[test]
    fn expired_lease_is_requeued_as_next_attempt() {
        let mut s = MemStore::default();
        s.put_attempt(&running("T001", 1, 100)).unwrap();
        let actions = recover_orphans(&mut s, 10_000).unwrap();
        assert_eq!(
            actions,
            vec![RecoveryAction::Requeued {
                task_id: TaskId::new("T001").unwrap(),
                from_attempt: 1,
                to_attempt: 2,
            }]
        );
        let old = s.get_attempt(&TaskId::new("T001").unwrap(), 1).unwrap();
        assert_eq!(old.state, TaskState::Cancelled);
        assert_eq!(old.version, 4);
        let next = s.get_attempt(&TaskId::new("T001").unwrap(), 2).unwrap();
        assert_eq!(next.state, TaskState::Ready);
        assert_eq!(next.attempt, 2);
    }

    #[test]
    fn live_lease_is_left_alone() {
        let mut s = MemStore::default();
        s.put_attempt(&running("T002", 1, 99_999)).unwrap();
        let actions = recover_orphans(&mut s, 10_000).unwrap();
        assert_eq!(
            actions,
            vec![RecoveryAction::LeaseLive {
                task_id: TaskId::new("T002").unwrap(),
                attempt: 1,
            }]
        );
        let r = s.get_attempt(&TaskId::new("T002").unwrap(), 1).unwrap();
        assert_eq!(r.state, TaskState::Running);
        assert_eq!(r.version, 3); // untouched
    }

    #[test]
    fn cas_race_yields_already_settled() {
        struct RacingStore(MemStore);
        impl DurableState for RacingStore {
            fn get_attempt(&self, t: &TaskId, a: u32) -> Option<AttemptRecord> {
                self.0.get_attempt(t, a)
            }
            fn put_attempt(&mut self, r: &AttemptRecord) -> Result<()> {
                self.0.put_attempt(r)
            }
            fn compare_and_swap(&mut self, _: &AttemptRecord, _: u64) -> Result<bool> {
                Ok(false) // someone else won the race
            }
            fn running_attempts(&self) -> Vec<AttemptRecord> {
                self.0.running_attempts()
            }
        }
        let mut s = RacingStore(MemStore::default());
        s.put_attempt(&running("T003", 1, 100)).unwrap();
        let actions = recover_orphans(&mut s, 10_000).unwrap();
        assert_eq!(
            actions,
            vec![RecoveryAction::AlreadySettled {
                task_id: TaskId::new("T003").unwrap(),
                attempt: 1,
            }]
        );
        // No phantom requeue was written.
        assert!(s.get_attempt(&TaskId::new("T003").unwrap(), 2).is_none());
    }

    #[test]
    fn empty_store_recovers_nothing() {
        let mut s = MemStore::default();
        assert_eq!(recover_orphans(&mut s, 10_000).unwrap(), vec![]);
    }

    #[test]
    fn lease_boundary_is_expired() {
        let r = running("T004", 1, 10_000);
        assert!(r.lease_expired(10_000));
        assert!(!r.lease_expired(9_999));
    }
}
