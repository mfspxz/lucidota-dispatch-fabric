//! T083 — hash-chained event log.
//!
//! Each record carries a strictly increasing sequence number `seq` (starting
//! at 0), the `prev_hash` of the record before it, the event itself, and a
//! `record_hash` computed as:
//!
//! ```text
//! record_hash = sha256( seq_le_bytes || prev_hash_bytes || canonical_event_bytes )
//! ```
//!
//! where `seq_le_bytes` is the 8-byte little-endian encoding of `seq`,
//! `prev_hash_bytes` is the 32 raw bytes of the previous record's hash
//! (all-zero for the genesis record, `seq == 0`), and
//! `canonical_event_bytes` is the event's canonical JSON.
//!
//! ## Detection, not prevention
//!
//! Hash chaining **detects** alteration; it does **not prevent** it. Anyone
//! with write access to the log can recompute a valid chain after tampering.
//! The chain makes tampering *evident* to an independent verifier holding
//! (or having independently recorded) a later record hash — a mismatch
//! between the recomputed chain and a known-good anchor proves the log was
//! altered after the anchor was taken. It is a tripwire, not a lock: pair
//! it with independent anchor publication (e.g. periodic external
//! timestamping) if prevention-level guarantees are required.

use fabric_types::{FabricError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::events::FabricEvent;

/// 32 raw hash bytes, rendered as `sha256:<hex>` for storage/display.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RecordHash(pub String);

impl RecordHash {
    /// The genesis anchor: all-zero bytes, rendered as `sha256:<64 zeros>`.
    pub fn genesis() -> Self {
        RecordHash(format!("sha256:{}", "0".repeat(64)))
    }

    /// Parse a `sha256:<64 hex>` hash. Rejects malformed input.
    pub fn parse(s: &str) -> Result<Self> {
        let hex = s.strip_prefix("sha256:").ok_or_else(|| {
            FabricError::InvalidInput {
                field: "record_hash",
                reason: "must start with 'sha256:'".to_string(),
            }
        })?;
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(FabricError::InvalidInput {
                field: "record_hash",
                reason: "must be 'sha256:' + 64 hex chars".to_string(),
            });
        }
        Ok(RecordHash(format!("sha256:{}", hex.to_ascii_lowercase())))
    }

    /// Raw 32 bytes of the hash (panics only if constructed outside `parse`
    /// / `compute`, which both enforce the invariant).
    pub fn raw_bytes(&self) -> [u8; 32] {
        let hex = &self.0["sha256:".len()..];
        let mut out = [0u8; 32];
        for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
            out[i] = u8::from_str_radix(std::str::from_utf8(chunk).unwrap(), 16).unwrap();
        }
        out
    }
}

/// One hash-chained event record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecord {
    /// Strictly increasing from 0 within one log.
    pub seq: u64,
    /// Hash of the previous record; the genesis anchor for `seq == 0`.
    pub prev_hash: RecordHash,
    pub event: FabricEvent,
    /// sha256(seq_le || prev_hash_bytes || canonical_event).
    pub record_hash: RecordHash,
}

/// Compute the record hash per the module-docs formula.
pub fn compute_record_hash(seq: u64, prev_hash: &RecordHash, event: &FabricEvent) -> RecordHash {
    let mut hasher = Sha256::new();
    hasher.update(seq.to_le_bytes());
    hasher.update(prev_hash.raw_bytes());
    hasher.update(event.canonical_json());
    let digest = hasher.finalize();
    RecordHash(format!("sha256:{}", hex_encode(&digest)))
}

/// An append-only event log with monotonic sequence numbers and hash
/// chaining. Not a durable store — serialize the records and persist them
/// with the caller of your choice.
#[derive(Debug, Default)]
pub struct EventLog {
    records: Vec<EventRecord>,
}

impl EventLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a log from records read off disk, verifying the whole chain.
    ///
    /// This is the choke point for untrusted input: any `EventLog`
    /// reconstructed from persisted bytes must pass through here, so a
    /// tampered log can never become a live in-memory log. Fresh logs built
    /// with `new()` + `append()` are verified-by-construction and do not
    /// need this path.
    ///
    /// Trust statement: the hash chain detects alteration of recorded
    /// events; it does not authenticate the writer — anyone with write
    /// access could recompute a valid chain. It is a tripwire, not a lock.
    pub fn from_records(records: Vec<EventRecord>) -> Result<Self> {
        let log = Self { records };
        log.verify()?;
        Ok(log)
    }

    /// Append an event. The record's seq is the next monotonic number and
    /// its prev_hash is the current head. Returns the appended record.
    pub fn append(&mut self, event: FabricEvent) -> EventRecord {
        let seq = self.records.len() as u64;
        let prev_hash = self
            .records
            .last()
            .map(|r| r.record_hash.clone())
            .unwrap_or_else(RecordHash::genesis);
        let record_hash = compute_record_hash(seq, &prev_hash, &event);
        let record = EventRecord {
            seq,
            prev_hash,
            event,
            record_hash,
        };
        self.records.push(record.clone());
        record
    }

    pub fn records(&self) -> &[EventRecord] {
        &self.records
    }

    pub fn head_hash(&self) -> RecordHash {
        self.records
            .last()
            .map(|r| r.record_hash.clone())
            .unwrap_or_else(RecordHash::genesis)
    }

    /// Verify the whole chain: seq strictly increasing from 0, each
    /// prev_hash matching the previous record_hash, and every record_hash
    /// recomputing correctly. Returns the first defect found.
    pub fn verify(&self) -> Result<()> {
        let mut expected_prev = RecordHash::genesis();
        for (i, record) in self.records.iter().enumerate() {
            if record.seq != i as u64 {
                return Err(FabricError::InvalidInput {
                    field: "seq",
                    reason: format!(
                        "non-monotonic sequence at index {}: expected {}, found {}",
                        i, i, record.seq
                    ),
                });
            }
            if record.prev_hash != expected_prev {
                return Err(FabricError::InvalidInput {
                    field: "prev_hash",
                    reason: format!("prev_hash mismatch at seq {}", record.seq),
                });
            }
            let recomputed = compute_record_hash(record.seq, &record.prev_hash, &record.event);
            if recomputed != record.record_hash {
                return Err(FabricError::InvalidInput {
                    field: "record_hash",
                    reason: format!(
                        "record_hash mismatch at seq {}: log was altered",
                        record.seq
                    ),
                });
            }
            expected_prev = record.record_hash.clone();
        }
        Ok(())
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

/// Build a minimal event for tests and callers that only need a chain entry.
#[cfg(test)]
pub(crate) fn test_event(ts: u64) -> FabricEvent {
    use crate::events::{EventPayload, FabricEvent};
    use fabric_types::TaskId;
    FabricEvent::new(
        ts,
        TaskId::new("T001").unwrap(),
        1,
        EventPayload::Release {
            reason: "test".to_string(),
        },
    )
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genesis_record_links_to_zero_anchor() {
        let mut log = EventLog::new();
        let r = log.append(test_event(1));
        assert_eq!(r.seq, 0);
        assert_eq!(r.prev_hash, RecordHash::genesis());
        assert_eq!(log.verify(), Ok(()));
    }

    #[test]
    fn seq_is_strictly_increasing_and_chained() {
        let mut log = EventLog::new();
        let r0 = log.append(test_event(1));
        let r1 = log.append(test_event(2));
        let r2 = log.append(test_event(3));
        assert_eq!((r0.seq, r1.seq, r2.seq), (0, 1, 2));
        assert_eq!(r1.prev_hash, r0.record_hash);
        assert_eq!(r2.prev_hash, r1.record_hash);
        assert_eq!(log.verify(), Ok(()));
    }

    #[test]
    fn head_hash_of_empty_log_is_genesis() {
        let log = EventLog::new();
        assert_eq!(log.head_hash(), RecordHash::genesis());
    }

    #[test]
    fn tampered_event_is_detected() {
        let mut log = EventLog::new();
        log.append(test_event(1));
        log.append(test_event(2));
        // Alter the payload of the first record's event in place.
        let mut tampered = log.records.clone();
        tampered[0].event.timestamp_ms = 999;
        let tampered_log = EventLog { records: tampered };
        let err = tampered_log.verify().unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }));
        assert!(format!("{:?}", err).contains("record_hash mismatch"));
    }

    #[test]
    fn tampered_seq_is_detected() {
        let mut log = EventLog::new();
        log.append(test_event(1));
        log.append(test_event(2));
        let mut tampered = log.records.clone();
        tampered[1].seq = 7;
        let tampered_log = EventLog { records: tampered };
        assert!(tampered_log.verify().is_err());
    }

    #[test]
    fn broken_prev_hash_link_is_detected() {
        let mut log = EventLog::new();
        log.append(test_event(1));
        log.append(test_event(2));
        let mut tampered = log.records.clone();
        tampered[1].prev_hash = RecordHash::genesis();
        let tampered_log = EventLog { records: tampered };
        let err = tampered_log.verify().unwrap_err();
        assert!(format!("{:?}", err).contains("prev_hash mismatch"));
    }

    #[test]
    fn record_hash_is_deterministic() {
        let h1 = compute_record_hash(3, &RecordHash::genesis(), &test_event(42));
        let h2 = compute_record_hash(3, &RecordHash::genesis(), &test_event(42));
        assert_eq!(h1, h2);
        // Different seq -> different hash.
        let h3 = compute_record_hash(4, &RecordHash::genesis(), &test_event(42));
        assert_ne!(h1, h3);
    }

    #[test]
    fn hash_roundtrips_raw_bytes() {
        let h = compute_record_hash(0, &RecordHash::genesis(), &test_event(1));
        let parsed = RecordHash::parse(&h.0).unwrap();
        assert_eq!(parsed, h);
        assert_eq!(parsed.raw_bytes().len(), 32);
    }

    #[test]
    fn parse_rejects_malformed_hashes() {
        assert!(RecordHash::parse("bogus").is_err());
        assert!(RecordHash::parse("sha256:abc").is_err());
        assert!(RecordHash::parse(&format!("sha256:{}", "g".repeat(64))).is_err());
    }

    #[test]
    fn records_serde_roundtrip() {
        let mut log = EventLog::new();
        log.append(test_event(1));
        log.append(test_event(2));
        let json = serde_json::to_vec(&log.records).unwrap();
        let back: Vec<EventRecord> = serde_json::from_slice(&json).unwrap();
        assert_eq!(back, log.records);
    }

    #[test]
    fn from_records_accepts_valid_chain() {
        let mut log = EventLog::new();
        log.append(test_event(1));
        log.append(test_event(2));
        let records = log.records().to_vec();
        let head = log.head_hash();
        let rebuilt = EventLog::from_records(records).unwrap();
        assert_eq!(rebuilt.head_hash(), head);
        assert_eq!(rebuilt.records().len(), 2);
        // Appends continue the chain at the right seq.
        let mut rebuilt = rebuilt;
        let r = rebuilt.append(test_event(3));
        assert_eq!(r.seq, 2);
        assert_eq!(rebuilt.verify(), Ok(()));
    }

    #[test]
    fn from_records_rejects_tampered_chain() {
        let mut log = EventLog::new();
        log.append(test_event(1));
        log.append(test_event(2));
        let mut records = log.records().to_vec();
        // Flip one byte of the first record's event payload.
        records[0].event.timestamp_ms = 999;
        let err = EventLog::from_records(records).unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }));
        assert!(format!("{:?}", err).contains("record_hash mismatch"));
    }

    #[test]
    fn from_records_rejects_broken_seq() {
        let mut log = EventLog::new();
        log.append(test_event(1));
        let mut records = log.records().to_vec();
        records[0].seq = 5;
        assert!(EventLog::from_records(records).is_err());
    }

    #[test]
    fn from_records_accepts_empty() {
        let log = EventLog::from_records(Vec::new()).unwrap();
        assert_eq!(log.head_hash(), RecordHash::genesis());
    }
}
