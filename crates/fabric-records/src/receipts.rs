//! T084 — transactional attempt receipts.
//!
//! A task-attempt receipt becomes durable (visible to receipt readers) only
//! after an explicit `commit_attempt` that fsyncs the write-ahead log.
//! Crash semantics: an attempt that was begun but never committed is NOT a
//! receipt — after a crash it is invisible to receipt readers, so a crash
//! can never record completion without evidence.
//!
//! Storage is a newline-delimited JSON write-ahead log (`attempts.wal`) in
//! the store directory. No SQLite. A torn tail frame on reopen is ignored;
//! a corrupt mid-log frame is a typed error, never silently skipped.
//!
//! The [`AttemptJournal`] trait is the integration seam for orphan recovery
//! (the batch-2 `DurableState` trait plugs into it): it exposes begun
//! records so recovery machinery can find RUNNING attempts and CAS them to
//! Cancelled, while [`ReceiptStore::get_receipt`] only ever surfaces
//! committed receipts.

use fabric_types::{FabricError, Result, TaskId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Compute `sha256:` + lowercase hex of bytes.
///
/// This matches the frozen T081 `digest_bytes` contract byte-for-byte; the
/// integrator will rewire this call site to `store::digest_bytes` when T081
/// lands. (`sha2` is already a declared dependency of this crate.)
pub(crate) fn sha256_prefixed(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut s = String::with_capacity(7 + 64);
    s.push_str("sha256:");
    for b in digest {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn io_err(e: std::io::Error, what: &'static str) -> FabricError {
    // The frozen T010 error enum has no storage variant; TransportFailure is
    // the closest typed carrier for local IO failure. Documented, not hidden.
    FabricError::TransportFailure(format!("{}: {}", what, e))
}

/// One piece of evidence attached to an attempt: a name plus the content
/// digest (`algo:hex`) of the evidence bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub name: String,
    pub digest: String,
}

/// Lifecycle status of an attempt record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AttemptStatus {
    /// Begun but not committed. Visible to the journal seam only; invisible
    /// to receipt readers, including after a crash.
    Running,
    /// Cancelled via the journal seam (orphan recovery). Terminal.
    Cancelled,
    /// Committed with an fsync. Visible to receipt readers. Terminal.
    Committed,
}

/// The durable record for one task attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub task_id: TaskId,
    pub attempt: u32,
    pub status: AttemptStatus,
    pub evidence: Vec<EvidenceRef>,
    /// Set only when `status == Committed`: sha256 over the canonical
    /// `(task_id, attempt, sorted evidence)` JSON.
    pub receipt_digest: Option<String>,
    /// Monotonic sequence of the last journal frame that touched this record.
    /// CAS callers use this to detect races.
    pub last_seq: u64,
}

impl AttemptRecord {
    fn key(&self) -> (TaskId, u32) {
        (self.task_id.clone(), self.attempt)
    }
}

/// Integration seam for orphan recovery.
///
/// The batch-2 `DurableState` trait (lease-based orphan recovery) plugs into
/// this trait: `get_attempt` finds RUNNING attempts after a restart,
/// `compare_and_swap` moves one to Cancelled exactly once even under races.
/// Unlike the receipt API, the journal exposes records in every status.
pub trait AttemptJournal {
    /// Latest record for `(task_id, attempt)` in any status, or `None`.
    fn get_attempt(&self, task_id: &TaskId, attempt: u32) -> Result<Option<AttemptRecord>>;
    /// Durably replace the record (append + fsync). The record's own key
    /// identifies the slot.
    fn put_attempt(&mut self, record: &AttemptRecord) -> Result<()>;
    /// Replace only if the current record equals `expected` (full-record
    /// comparison, including `last_seq`). Returns `true` when the swap
    /// happened, `false` on a race. Durable (append + fsync).
    fn compare_and_swap(&mut self, expected: &AttemptRecord, new: &AttemptRecord) -> Result<bool>;
}

/// Write-ahead log frames. The log is append-only; frames are the source of
/// truth on reopen.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Frame {
    Begin {
        seq: u64,
        task_id: TaskId,
        attempt: u32,
    },
    Evidence {
        seq: u64,
        task_id: TaskId,
        attempt: u32,
        name: String,
        digest: String,
    },
    Commit {
        seq: u64,
        task_id: TaskId,
        attempt: u32,
        receipt_digest: String,
    },
    /// Raw journal write (the [`AttemptJournal`] seam).
    Set { seq: u64, record: AttemptRecord },
}

const WAL_NAME: &str = "attempts.wal";

/// Transactional receipt store backed by a write-ahead log.
pub struct ReceiptStore {
    wal_path: PathBuf,
    wal: File,
    next_seq: u64,
    records: HashMap<(TaskId, u32), AttemptRecord>,
}

impl ReceiptStore {
    /// Open (creating) the store in `dir`, replaying the write-ahead log.
    pub fn open(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir).map_err(|e| io_err(e, "create store dir"))?;
        let wal_path = dir.join(WAL_NAME);
        let mut store = ReceiptStore {
            wal_path: wal_path.clone(),
            wal: OpenOptions::new()
                .create(true)
                .append(true)
                .open(&wal_path)
                .map_err(|e| io_err(e, "open journal"))?,
            next_seq: 1,
            records: HashMap::new(),
        };
        store.replay()?;
        Ok(store)
    }

    /// Replay the log. A torn final frame is ignored; a corrupt non-final
    /// frame is an error.
    fn replay(&mut self) -> Result<()> {
        let file = File::open(&self.wal_path).map_err(|e| io_err(e, "replay journal"))?;
        let reader = BufReader::new(file);
        let lines: Vec<std::io::Result<String>> = reader.lines().collect();
        let total = lines.len();
        for (i, line) in lines.into_iter().enumerate() {
            let line = line.map_err(|e| io_err(e, "read journal line"))?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Frame>(&line) {
                Ok(frame) => self.apply(frame),
                Err(_) if i + 1 == total => {
                    // Torn tail: the last frame was partially written by a
                    // crash. Ignore it; it was never fsynced as complete.
                }
                Err(_) => {
                    return Err(FabricError::MalformedOutput(format!(
                        "journal frame {} does not parse",
                        i + 1
                    )));
                }
            }
        }
        Ok(())
    }

    fn apply(&mut self, frame: Frame) {
        match frame {
            Frame::Begin {
                seq,
                task_id,
                attempt,
            } => {
                self.next_seq = self.next_seq.max(seq + 1);
                self.records.insert(
                    (task_id.clone(), attempt),
                    AttemptRecord {
                        task_id,
                        attempt,
                        status: AttemptStatus::Running,
                        evidence: Vec::new(),
                        receipt_digest: None,
                        last_seq: seq,
                    },
                );
            }
            Frame::Evidence {
                seq,
                task_id,
                attempt,
                name,
                digest,
            } => {
                self.next_seq = self.next_seq.max(seq + 1);
                if let Some(rec) = self.records.get_mut(&(task_id, attempt)) {
                    if rec.status == AttemptStatus::Running {
                        rec.evidence.push(EvidenceRef { name, digest });
                        rec.last_seq = seq;
                    }
                }
            }
            Frame::Commit {
                seq,
                task_id,
                attempt,
                receipt_digest,
            } => {
                self.next_seq = self.next_seq.max(seq + 1);
                if let Some(rec) = self.records.get_mut(&(task_id, attempt)) {
                    if rec.status == AttemptStatus::Running {
                        rec.status = AttemptStatus::Committed;
                        rec.receipt_digest = Some(receipt_digest);
                        rec.last_seq = seq;
                    }
                }
            }
            Frame::Set { seq, record } => {
                self.next_seq = self.next_seq.max(seq + 1);
                let mut record = record;
                record.last_seq = seq;
                self.records.insert(record.key(), record);
            }
        }
    }

    /// Append one frame and flush (no fsync: the frame is not yet durable).
    fn append(&mut self, op: FrameOp) -> Result<u64> {
        let seq = self.next_seq;
        self.next_seq += 1;
        let frame = op.with_seq(seq);
        let mut line = serde_json::to_vec(&frame).map_err(|e| {
            FabricError::MalformedOutput(format!("journal frame serialize: {}", e))
        })?;
        line.push(b'\n');
        self.wal.write_all(&line).map_err(|e| io_err(e, "append journal"))?;
        self.wal.flush().map_err(|e| io_err(e, "flush journal"))?;
        Ok(seq)
    }

    /// Append one frame and make it durable.
    fn append_durable(&mut self, op: FrameOp) -> Result<u64> {
        let seq = self.append(op)?;
        self.wal.sync_all().map_err(|e| io_err(e, "fsync journal"))?;
        Ok(seq)
    }

    /// Begin an attempt. The attempt is journal-visible but produces no
    /// receipt until committed.
    pub fn begin_attempt(&mut self, task_id: &TaskId, attempt: u32) -> Result<()> {
        let key = (task_id.clone(), attempt);
        if self.records.contains_key(&key) {
            return Err(FabricError::Conflict(format!(
                "attempt {}#{} already begun",
                task_id.as_str(),
                attempt
            )));
        }
        let seq = self.append(FrameOp::Begin {
            task_id: task_id.clone(),
            attempt,
        })?;
        self.records.insert(
            key,
            AttemptRecord {
                task_id: task_id.clone(),
                attempt,
                status: AttemptStatus::Running,
                evidence: Vec::new(),
                receipt_digest: None,
                last_seq: seq,
            },
        );
        Ok(())
    }

    /// Attach one evidence reference to a running attempt.
    pub fn record_evidence(
        &mut self,
        task_id: &TaskId,
        attempt: u32,
        name: &str,
        digest: &str,
    ) -> Result<()> {
        let key = (task_id.clone(), attempt);
        let rec = self.records.get(&key).ok_or_else(|| FabricError::InvalidInput {
            field: "attempt",
            reason: "unknown attempt".to_string(),
        })?;
        if rec.status != AttemptStatus::Running {
            return Err(FabricError::Conflict(format!(
                "attempt {}#{} is not running",
                task_id.as_str(),
                attempt
            )));
        }
        let seq = self.append(FrameOp::Evidence {
            task_id: task_id.clone(),
            attempt,
            name: name.to_string(),
            digest: digest.to_string(),
        })?;
        let rec = self.records.get_mut(&key).expect("checked above");
        rec.evidence.push(EvidenceRef {
            name: name.to_string(),
            digest: digest.to_string(),
        });
        rec.last_seq = seq;
        Ok(())
    }

    /// Commit an attempt: the receipt becomes durable and visible to readers.
    ///
    /// Requires the attempt to be Running with at least one evidence record —
    /// a crash (or a caller) can never record completion without evidence.
    /// Returns the receipt digest.
    pub fn commit_attempt(&mut self, task_id: &TaskId, attempt: u32) -> Result<String> {
        let key = (task_id.clone(), attempt);
        let rec = self.records.get(&key).ok_or_else(|| FabricError::InvalidInput {
            field: "attempt",
            reason: "unknown attempt".to_string(),
        })?;
        if rec.status != AttemptStatus::Running {
            return Err(FabricError::Conflict(format!(
                "attempt {}#{} is not running",
                task_id.as_str(),
                attempt
            )));
        }
        if rec.evidence.is_empty() {
            return Err(FabricError::InvalidInput {
                field: "evidence",
                reason: "commit requires at least one evidence record".to_string(),
            });
        }
        let digest = receipt_digest(task_id, attempt, &rec.evidence);
        let seq = self.append_durable(FrameOp::Commit {
            task_id: task_id.clone(),
            attempt,
            receipt_digest: digest.clone(),
        })?;
        let rec = self.records.get_mut(&key).expect("checked above");
        rec.status = AttemptStatus::Committed;
        rec.receipt_digest = Some(digest.clone());
        rec.last_seq = seq;
        Ok(digest)
    }

    /// The committed receipt for an attempt, or `None` when there is none.
    /// Uncommitted attempts are invisible here by design.
    pub fn get_receipt(&self, task_id: &TaskId, attempt: u32) -> Option<&AttemptRecord> {
        let rec = self.records.get(&(task_id.clone(), attempt))?;
        (rec.status == AttemptStatus::Committed).then_some(rec)
    }

    /// All committed receipts, in deterministic (task, attempt) order.
    pub fn committed(&self) -> Vec<&AttemptRecord> {
        let mut out: Vec<&AttemptRecord> = self
            .records
            .values()
            .filter(|r| r.status == AttemptStatus::Committed)
            .collect();
        out.sort_by(|a, b| {
            (a.task_id.as_str(), a.attempt).cmp(&(b.task_id.as_str(), b.attempt))
        });
        out
    }
}

/// Canonical receipt digest: sha256 over JSON with sorted evidence.
fn receipt_digest(task_id: &TaskId, attempt: u32, evidence: &[EvidenceRef]) -> String {
    let mut ev: Vec<serde_json::Value> = evidence
        .iter()
        .map(|e| serde_json::json!({"name": e.name, "digest": e.digest}))
        .collect();
    ev.sort_by_key(|a| a.to_string());
    let canonical = serde_json::json!({
        "task_id": task_id.as_str(),
        "attempt": attempt,
        "evidence": ev,
    });
    let bytes = serde_json::to_vec(&canonical).expect("json serialization cannot fail");
    sha256_prefixed(&bytes)
}

/// Frame constructors without sequence numbers (assigned at append time).
enum FrameOp {
    Begin { task_id: TaskId, attempt: u32 },
    Evidence {
        task_id: TaskId,
        attempt: u32,
        name: String,
        digest: String,
    },
    Commit {
        task_id: TaskId,
        attempt: u32,
        receipt_digest: String,
    },
    Set { record: AttemptRecord },
}

impl FrameOp {
    fn with_seq(self, seq: u64) -> Frame {
        match self {
            FrameOp::Begin { task_id, attempt } => Frame::Begin { seq, task_id, attempt },
            FrameOp::Evidence {
                task_id,
                attempt,
                name,
                digest,
            } => Frame::Evidence {
                seq,
                task_id,
                attempt,
                name,
                digest,
            },
            FrameOp::Commit {
                task_id,
                attempt,
                receipt_digest,
            } => Frame::Commit {
                seq,
                task_id,
                attempt,
                receipt_digest,
            },
            FrameOp::Set { record } => Frame::Set { seq, record },
        }
    }
}

impl AttemptJournal for ReceiptStore {
    fn get_attempt(&self, task_id: &TaskId, attempt: u32) -> Result<Option<AttemptRecord>> {
        Ok(self.records.get(&(task_id.clone(), attempt)).cloned())
    }

    fn put_attempt(&mut self, record: &AttemptRecord) -> Result<()> {
        let seq = self.append_durable(FrameOp::Set {
            record: record.clone(),
        })?;
        let mut record = record.clone();
        record.last_seq = seq;
        self.records.insert(record.key(), record);
        Ok(())
    }

    fn compare_and_swap(
        &mut self,
        expected: &AttemptRecord,
        new: &AttemptRecord,
    ) -> Result<bool> {
        if expected.key() != new.key() {
            return Err(FabricError::InvalidInput {
                field: "attempt",
                reason: "CAS records must address the same attempt".to_string(),
            });
        }
        match self.records.get(&expected.key()) {
            Some(current) if current == expected => {
                self.put_attempt(new)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn scratch_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rec3-receipts-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn tid(s: &str) -> TaskId {
        TaskId::new(s).unwrap()
    }

    #[test]
    fn commit_makes_receipt_durable_across_reopen() {
        let dir = scratch_dir();
        let task = tid("t-durable-1");
        let digest = {
            let mut s = ReceiptStore::open(&dir).unwrap();
            s.begin_attempt(&task, 1).unwrap();
            s.record_evidence(&task, 1, "out.json", "sha256:aaa").unwrap();
            s.record_evidence(&task, 1, "log.txt", "sha256:bbb").unwrap();
            s.commit_attempt(&task, 1).unwrap()
        };
        let s = ReceiptStore::open(&dir).unwrap();
        let rec = s.get_receipt(&task, 1).expect("committed receipt survives");
        assert_eq!(rec.status, AttemptStatus::Committed);
        assert_eq!(rec.receipt_digest.as_deref(), Some(digest.as_str()));
        assert_eq!(rec.evidence.len(), 2);
        assert!(digest.starts_with("sha256:"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn uncommitted_attempt_invisible_after_crash() {
        let dir = scratch_dir();
        let task = tid("t-crash-1");
        {
            let mut s = ReceiptStore::open(&dir).unwrap();
            s.begin_attempt(&task, 1).unwrap();
            s.record_evidence(&task, 1, "out.json", "sha256:aaa").unwrap();
            s.begin_attempt(&task, 2).unwrap();
            s.record_evidence(&task, 2, "out.json", "sha256:bbb").unwrap();
            s.commit_attempt(&task, 2).unwrap();
            // Attempt 1 is never committed: dropping the store here is the
            // simulated crash.
        }
        let s = ReceiptStore::open(&dir).unwrap();
        assert!(
            s.get_receipt(&task, 1).is_none(),
            "uncommitted attempt must be invisible to receipt readers"
        );
        assert!(s.get_receipt(&task, 2).is_some(), "committed receipt survives");
        assert!(s.committed().iter().all(|r| r.attempt == 2));
        // The journal seam still exposes the begun record so orphan
        // recovery can find and cancel it.
        let rec = s.get_attempt(&task, 1).unwrap().expect("journal sees begun");
        assert_eq!(rec.status, AttemptStatus::Running);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_requires_evidence() {
        let dir = scratch_dir();
        let mut s = ReceiptStore::open(&dir).unwrap();
        let task = tid("t-noev-1");
        s.begin_attempt(&task, 1).unwrap();
        let err = s.commit_attempt(&task, 1).unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }));
        assert!(s.get_receipt(&task, 1).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn double_begin_is_conflict() {
        let dir = scratch_dir();
        let mut s = ReceiptStore::open(&dir).unwrap();
        let task = tid("t-dbl-1");
        s.begin_attempt(&task, 1).unwrap();
        assert!(matches!(
            s.begin_attempt(&task, 1),
            Err(FabricError::Conflict(_))
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cas_detects_race() {
        let dir = scratch_dir();
        let mut s = ReceiptStore::open(&dir).unwrap();
        let task = tid("t-cas-1");
        s.begin_attempt(&task, 1).unwrap();
        let seen = s.get_attempt(&task, 1).unwrap().unwrap();
        // Someone else moves it first.
        let mut moved = seen.clone();
        moved.status = AttemptStatus::Cancelled;
        assert!(s.compare_and_swap(&seen, &moved).unwrap());
        // Our stale view no longer matches.
        let mut stale_try = seen.clone();
        stale_try.status = AttemptStatus::Cancelled;
        assert!(!s.compare_and_swap(&seen, &stale_try).unwrap());
        // A cross-attempt CAS is a caller bug, not a race.
        let other = AttemptRecord {
            attempt: 2,
            ..seen.clone()
        };
        assert!(matches!(
            s.compare_and_swap(&seen, &other),
            Err(FabricError::InvalidInput { .. })
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn torn_tail_frame_ignored_on_reopen() {
        let dir = scratch_dir();
        let task = tid("t-torn-1");
        {
            let mut s = ReceiptStore::open(&dir).unwrap();
            s.begin_attempt(&task, 1).unwrap();
            s.record_evidence(&task, 1, "out.json", "sha256:aaa").unwrap();
            s.commit_attempt(&task, 1).unwrap();
            // Simulate a crash mid-write: append a partial frame with no
            // trailing newline and no fsync.
            use std::io::Write;
            let mut f = OpenOptions::new().append(true).open(dir.join(WAL_NAME)).unwrap();
            f.write_all(b"{\"op\":\"begin\",\"seq\":999,\"task_id\":\"t-tor").unwrap();
        }
        let s = ReceiptStore::open(&dir).unwrap();
        assert!(s.get_receipt(&task, 1).is_some());
        assert!(s.get_attempt(&tid("t-tor"), 999).unwrap().is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn receipt_digest_covers_evidence_and_is_stable() {
        let d1 = receipt_digest(&tid("t-x"), 1, &[EvidenceRef {
            name: "a".into(),
            digest: "sha256:1".into(),
        }]);
        let d2 = receipt_digest(&tid("t-x"), 1, &[EvidenceRef {
            name: "a".into(),
            digest: "sha256:1".into(),
        }]);
        assert_eq!(d1, d2);
        let d3 = receipt_digest(&tid("t-x"), 1, &[EvidenceRef {
            name: "a".into(),
            digest: "sha256:2".into(),
        }]);
        assert_ne!(d1, d3);
        // Evidence order does not matter.
        let d4 = receipt_digest(
            &tid("t-x"),
            1,
            &[
                EvidenceRef { name: "b".into(), digest: "sha256:2".into() },
                EvidenceRef { name: "a".into(), digest: "sha256:1".into() },
            ],
        );
        let d5 = receipt_digest(
            &tid("t-x"),
            1,
            &[
                EvidenceRef { name: "a".into(), digest: "sha256:1".into() },
                EvidenceRef { name: "b".into(), digest: "sha256:2".into() },
            ],
        );
        assert_eq!(d4, d5);
    }
}
