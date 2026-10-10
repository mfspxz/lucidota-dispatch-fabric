//! DRV-02: the dead-drop protocol.
//!
//! The driver and workers never meet. The driver writes dispatch envelopes
//! to `outbox/<envelope_digest>.json`; workers watch the outbox, do the
//! work in the workspace named by the envelope, and drop receipts at
//! `inbox/<envelope_digest>.receipt.json`. Handoffs occur by digest only —
//! the driver knows digests, not contents.
//!
//! Envelope files are immutable once written (the filename is the digest of
//! the canonical bytes, so any modification would change the name).

use crate::driver::DriverError;
use fabric_dispatch::DispatchEnvelope;
use fabric_records::store::digest_bytes;
use notify::{Config as NotifyConfig, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

/// Write side of the dead drop.
pub struct Outbox {
    dir: PathBuf,
}

impl Outbox {
    /// Create (or open) the outbox directory.
    pub fn new(dir: &Path) -> Result<Self, DriverError> {
        std::fs::create_dir_all(dir).map_err(|e| {
            DriverError::Backend(format!("cannot create outbox {}: {e}", dir.display()))
        })?;
        Ok(Self {
            dir: dir.to_path_buf(),
        })
    }

    /// Envelope file path for a digest, without writing.
    pub fn path_for(&self, envelope_digest: &str) -> PathBuf {
        self.dir.join(format!("{envelope_digest}.json"))
    }

    /// Write an envelope to the outbox. The bytes are the canonical JSON
    /// serialization (field declaration order, no maps) — the same bytes
    /// `envelope_digest()` was computed over. Writes go through a temp file
    /// plus atomic rename; an existing file for the same digest is byte-
    /// identical by construction and is left untouched (never modified).
    pub fn write_envelope(&self, envelope: &DispatchEnvelope) -> Result<PathBuf, DriverError> {
        let digest = envelope.envelope_digest();
        let path = self.path_for(&digest);
        if path.exists() {
            return Ok(path);
        }
        let bytes = serde_json::to_vec(envelope)
            .map_err(|e| DriverError::Backend(format!("envelope serialization failed: {e}")))?;
        write_atomic(&path, &bytes).map_err(|e| {
            DriverError::Backend(format!("cannot write outbox file {}: {e}", path.display()))
        })?;
        Ok(path)
    }
}

/// A receipt file read back from the inbox.
#[derive(Debug, Clone)]
pub struct ReceiptFile {
    /// Full path of the receipt file.
    pub path: PathBuf,
    /// Envelope digest this receipt claims to answer (from the filename).
    pub envelope_digest: String,
    /// Parsed receipt body.
    pub body: serde_json::Value,
    /// sha256 digest of the exact receipt file bytes.
    pub file_digest: String,
}

/// Read side of the dead drop.
pub struct Inbox {
    dir: PathBuf,
}

/// Maximum receipt file size: 8 MiB. Receipts are small JSON claim
/// envelopes (typically single-digit KiB). Anything larger is a worker bug
/// or a resource-exhaustion attempt: rejected with a typed error naming the
/// file, handled by the caller's quarantine path. Never read unbounded
/// into memory.
const MAX_RECEIPT_BYTES: u64 = 8 * 1024 * 1024;

/// A receipt file that exists but cannot be used: corrupt JSON, digest
/// linkage mismatch, or over the size cap. Quarantined by the caller —
/// it fails its task, never the run.
#[derive(Debug, Clone)]
pub struct ReceiptProblem {
    /// Envelope digest from the filename (the claim being answered).
    pub envelope_digest: String,
    pub path: PathBuf,
    /// Human-readable cause, naming the file.
    pub error: String,
}

impl Inbox {
    /// Create (or open) the inbox directory.
    pub fn new(dir: &Path) -> Result<Self, DriverError> {
        std::fs::create_dir_all(dir).map_err(|e| {
            DriverError::Backend(format!("cannot create inbox {}: {e}", dir.display()))
        })?;
        Ok(Self {
            dir: dir.to_path_buf(),
        })
    }

    /// Receipt file path for a digest, without reading.
    pub fn path_for(&self, envelope_digest: &str) -> PathBuf {
        self.dir.join(format!("{envelope_digest}.receipt.json"))
    }

    /// Wait for receipts, driven by filesystem events — never by polling.
    ///
    /// Watches the inbox directory with the OS file-notification facility
    /// (inotify on Linux via the `notify` crate) and rescans the expected
    /// digests on every event, plus one initial scan on entry so receipts
    /// that already exist are never missed. Spurious wakeups only cause an
    /// extra rescan; there is no `thread::sleep` anywhere in this wait path.
    ///
    /// Returns the receipts collected (possibly fewer than expected when the
    /// terminal deadline fires). `timeout_ms == 0` waits indefinitely — the
    /// caller owns liveness in that mode.
    ///
    /// Atomic-write assumption: workers publish receipts by writing a temp
    /// file and renaming it into place (see `write_atomic`, and the seeder's
    /// tmp+rename). A rename appears as a single create event for the final
    /// `*.receipt.json` name, so a file we observe under an expected name is
    /// complete — never a half-written stream.
    ///
    /// A receipt file that fails to parse, disagrees with its filename
    /// digest (linkage), or exceeds the size cap is a typed problem naming
    /// the file — quarantined, never silently skipped, never fatal to the
    /// run.
    ///
    /// Wait for receipts, driven by filesystem events — never by polling.
    ///
    /// Watches the inbox directory with the OS file-notification facility
    /// (inotify on Linux via the `notify` crate) and rescans the expected
    /// digests on every event, plus one initial scan on entry so receipts
    /// that arrived before the watch started are not missed.
    ///
    /// Returns the usable receipts alongside the quarantined problems. A
    /// malformed receipt (corrupt JSON, linkage mismatch, over the size
    /// cap) counts as *answered* — it appears in `problems`, and the
    /// caller fails that task rather than aborting the run. One buggy
    /// worker cannot DoS the run.
    pub fn wait_for_receipts(
        &self,
        expected_digests: &[String],
        timeout_ms: u64,
    ) -> Result<(Vec<ReceiptFile>, Vec<ReceiptProblem>), DriverError> {
        // The watcher must exist before the first scan: an event that lands
        // between watch-start and scan is queued in the channel and triggers
        // a rescan, so no wakeup is ever missed.
        let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
        let mut watcher =
            RecommendedWatcher::new(move |res| {
                let _ = tx.send(res);
            }, NotifyConfig::default())
            .map_err(|e| {
                DriverError::Backend(format!("cannot watch inbox {}: {e}", self.dir.display()))
            })?;
        watcher
            .watch(&self.dir, RecursiveMode::NonRecursive)
            .map_err(|e| {
                DriverError::Backend(format!("cannot watch inbox {}: {e}", self.dir.display()))
            })?;

        let start = Instant::now();
        let deadline = (timeout_ms > 0).then(|| start + Duration::from_millis(timeout_ms));

        loop {
            let (collected, problems) = self.scan(expected_digests);
            if collected.len() + problems.len() == expected_digests.len() {
                return Ok((collected, problems));
            }
            match deadline {
                None => {
                    // Indefinite wait: block on the next filesystem event.
                    // A watcher error event is infra noise, not a receipt;
                    // either way the loop rescans.
                    let _ = rx.recv().map_err(|e| {
                        DriverError::Backend(format!("inbox watcher died: {e}"))
                    })?;
                }
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Ok(self.scan(expected_digests));
                    }
                    match rx.recv_timeout(remaining) {
                        Ok(_) => {} // event (or watcher error) -> rescan
                        Err(RecvTimeoutError::Timeout) => {
                            // Terminal deadline: one final scan, then return
                            // whatever arrived.
                            return Ok(self.scan(expected_digests));
                        }
                        Err(RecvTimeoutError::Disconnected) => {
                            return Err(DriverError::Backend(
                                "inbox watcher died".to_string(),
                            ));
                        }
                    }
                }
            }
        }
    }

    /// One scan of the inbox for the expected digests. A present-but-
    /// unusable receipt becomes a `ReceiptProblem` — never silently skipped,
    /// never fatal to the scan.
    fn scan(&self, expected_digests: &[String]) -> (Vec<ReceiptFile>, Vec<ReceiptProblem>) {
        let mut collected: Vec<ReceiptFile> = Vec::new();
        let mut problems: Vec<ReceiptProblem> = Vec::new();
        for digest in expected_digests {
            let path = self.path_for(digest);
            if path.exists() {
                match self.read_receipt(&path, digest) {
                    Ok(r) => collected.push(r),
                    Err(e) => problems.push(ReceiptProblem {
                        envelope_digest: digest.clone(),
                        path: path.clone(),
                        error: e.to_string(),
                    }),
                }
            }
        }
        (collected, problems)
    }

    /// Read and validate one receipt file. The embedded `envelope_digest`
    /// must match the filename's digest (linkage check). Files over
    /// `MAX_RECEIPT_BYTES` are rejected before reading.
    fn read_receipt(&self, path: &Path, expected_digest: &str) -> Result<ReceiptFile, DriverError> {
        let len = path
            .metadata()
            .map_err(|e| {
                DriverError::Backend(format!("cannot stat receipt {}: {e}", path.display()))
            })?
            .len();
        if len > MAX_RECEIPT_BYTES {
            return Err(DriverError::Backend(format!(
                "receipt {} exceeds size cap ({} bytes > {} bytes)",
                path.display(),
                len,
                MAX_RECEIPT_BYTES
            )));
        }
        let bytes = std::fs::read(path).map_err(|e| {
            DriverError::Backend(format!("cannot read receipt {}: {e}", path.display()))
        })?;
        // Belt-and-braces: the file may have grown between the stat above
        // and the read. Never hold more than the cap in memory.
        if bytes.len() as u64 > MAX_RECEIPT_BYTES {
            return Err(DriverError::Backend(format!(
                "receipt {} grew past the size cap while reading ({} bytes > {} bytes)",
                path.display(),
                bytes.len(),
                MAX_RECEIPT_BYTES
            )));
        }
        let body: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| {
            DriverError::Backend(format!(
                "receipt {} fails to parse as JSON: {e}",
                path.display()
            ))
        })?;
        let embedded = body.get("envelope_digest").and_then(|v| v.as_str());
        if embedded != Some(expected_digest) {
            return Err(DriverError::Backend(format!(
                "receipt {} linkage failure: embedded envelope_digest {:?} != filename digest {}",
                path.display(),
                embedded,
                expected_digest
            )));
        }
        let file_digest = digest_bytes(&bytes).0;
        Ok(ReceiptFile {
            path: path.to_path_buf(),
            envelope_digest: expected_digest.to_string(),
            body,
            file_digest,
        })
    }
}

/// Atomic write: temp file in the same directory, then rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabric_dispatch::envelope::{create_envelope, EnvelopeLimits};
    use fabric_types::{AcceptancePredicate, EndpointId, Scope, TaskClass, TaskContract, TaskId};

    fn contract() -> TaskContract {
        TaskContract::builder(TaskId::new("T900").unwrap(), TaskClass::Implement, "do it")
            .spec_revision("rev-1")
            .inputs(vec!["in".to_string()])
            .outputs(vec!["out".to_string()])
            .scope(Scope::new(vec!["x".to_string()]).unwrap())
            .acceptance(vec![AcceptancePredicate::new("p", "d", true).unwrap()])
            .verifiers(vec!["v".to_string()])
            .build()
            .unwrap()
    }

    fn envelope() -> DispatchEnvelope {
        create_envelope(
            contract(),
            "sha256:deadbeef".to_string(),
            EndpointId::new("driver:local").unwrap(),
            EnvelopeLimits {
                timeout_ms: 1000,
                max_tokens: 100,
            },
            "ws-1".to_string(),
            serde_json::json!({"type": "object"}),
            1,
        )
        .unwrap()
    }

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("drv1-be-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn digest_matches_known_vector() {
        // fabric-records' canonical digest (sha2, not hand-rolled).
        assert_eq!(
            digest_bytes(b"abc").0,
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn outbox_write_is_idempotent_and_immutable() {
        let d = tmpdir("outbox");
        let ob = Outbox::new(&d.join("outbox")).unwrap();
        let e = envelope();
        let p1 = ob.write_envelope(&e).unwrap();
        let before = std::fs::read(&p1).unwrap();
        let p2 = ob.write_envelope(&e).unwrap();
        assert_eq!(p1, p2);
        assert_eq!(std::fs::read(&p2).unwrap(), before);
        // Filename is the envelope digest.
        assert_eq!(
            p1.file_name().unwrap().to_str().unwrap(),
            format!("{}.json", e.envelope_digest())
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn inbox_round_trip_with_preseeded_receipt() {
        let d = tmpdir("inbox");
        let ib = Inbox::new(&d.join("inbox")).unwrap();
        let digest = "sha256:abc123";
        let body = serde_json::json!({
            "envelope_digest": digest,
            "verdict": "pass",
            "artifact_digests": ["sha256:def456"],
        });
        std::fs::write(
            ib.path_for(digest),
            serde_json::to_vec(&body).unwrap(),
        )
        .unwrap();
        let (got, problems) = ib
            .wait_for_receipts(&[digest.to_string()], 5_000)
            .unwrap();
        assert_eq!(got.len(), 1);
        assert!(problems.is_empty());
        assert_eq!(got[0].envelope_digest, digest);
        assert_eq!(got[0].body["verdict"], "pass");
        assert!(got[0].file_digest.starts_with("sha256:"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn inbox_timeout_returns_what_arrived() {
        let d = tmpdir("timeout");
        let ib = Inbox::new(&d.join("inbox")).unwrap();
        let (got, problems) = ib
            .wait_for_receipts(&["sha256:never".to_string()], 120)
            .unwrap();
        assert!(got.is_empty());
        assert!(problems.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unparseable_receipt_is_quarantined_not_fatal() {
        // FIX-3: a malformed receipt no longer aborts the wait with Err.
        // It is quarantined as a ReceiptProblem naming the file; the run
        // continues and the caller fails that task.
        let d = tmpdir("poison");
        let ib = Inbox::new(&d.join("inbox")).unwrap();
        let digest = "sha256:bad1";
        std::fs::write(ib.path_for(digest), b"{not json").unwrap();
        let (got, problems) = ib
            .wait_for_receipts(&[digest.to_string()], 5_000)
            .unwrap();
        assert!(got.is_empty());
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].envelope_digest, digest);
        let msg = &problems[0].error;
        assert!(msg.contains("receipt") && msg.contains(".receipt.json"), "{msg}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn linkage_mismatch_is_quarantined() {
        let d = tmpdir("linkage");
        let ib = Inbox::new(&d.join("inbox")).unwrap();
        let digest = "sha256:real1";
        std::fs::write(
            ib.path_for(digest),
            serde_json::to_vec(&serde_json::json!({
                "envelope_digest": "sha256:other",
                "verdict": "pass",
            }))
            .unwrap(),
        )
        .unwrap();
        let (got, problems) = ib
            .wait_for_receipts(&[digest.to_string()], 5_000)
            .unwrap();
        assert!(got.is_empty());
        assert_eq!(problems.len(), 1);
        assert!(problems[0].error.contains("linkage"), "{}", problems[0].error);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn oversize_receipt_rejected_before_read() {
        // FIX-3: receipts are size-capped. A file one byte over the cap is
        // rejected with a typed error naming the file — never read
        // unbounded into memory.
        let d = tmpdir("oversize");
        let ib = Inbox::new(&d.join("inbox")).unwrap();
        let digest = "sha256:big1";
        let path = ib.path_for(digest);
        let big = vec![b'x'; (MAX_RECEIPT_BYTES + 1) as usize];
        std::fs::write(&path, &big).unwrap();
        let err = ib.read_receipt(&path, digest).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("exceeds size cap"), "{msg}");
        assert!(msg.contains(".receipt.json"), "{msg}");
        // And through the quarantine path:
        let (got, problems) = ib
            .wait_for_receipts(&[digest.to_string()], 5_000)
            .unwrap();
        assert!(got.is_empty());
        assert_eq!(problems.len(), 1);
        assert!(problems[0].error.contains("exceeds size cap"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn inbox_collects_receipt_arriving_after_wait_starts() {
        // The wait path is event-driven (inotify via `notify`), not polled:
        // there is no `thread::sleep` anywhere in `wait_for_receipts` — a
        // receipt published after the wait begins wakes the waiter through
        // the filesystem event channel. (Asserted by code inspection; the
        // wait below returns on the event, not on a timer tick.)
        let d = tmpdir("evented");
        let inbox_dir = d.join("inbox");
        let ib = Inbox::new(&inbox_dir).unwrap();
        let digest = "sha256:evented1".to_string();
        let target = ib.path_for(&digest);

        // Publisher: arrive 300ms after the wait starts, via atomic rename —
        // the same tmp+rename protocol the dead drop requires.
        let pub_digest = digest.clone();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let body = serde_json::json!({
                "envelope_digest": pub_digest,
                "verdict": "pass",
            });
            let tmp = target.with_extension("tmp");
            std::fs::write(&tmp, serde_json::to_vec(&body).unwrap()).unwrap();
            std::fs::rename(&tmp, &target).unwrap();
        });

        let start = Instant::now();
        let (got, problems) = ib.wait_for_receipts(std::slice::from_ref(&digest), 10_000).unwrap();
        let elapsed = start.elapsed();
        handle.join().unwrap();

        assert_eq!(got.len(), 1);
        assert!(problems.is_empty());
        assert_eq!(got[0].envelope_digest, digest);
        // Returned on the event, far short of the deadline.
        assert!(
            elapsed < Duration::from_secs(10),
            "wait did not return promptly: {elapsed:?}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
