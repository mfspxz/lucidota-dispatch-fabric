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
use std::path::{Path, PathBuf};
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

    /// Poll the inbox until every expected digest has a receipt or the
    /// timeout elapses. Returns the receipts that arrived (possibly fewer
    /// than expected on timeout). `timeout_ms == 0` waits indefinitely —
    /// the caller owns liveness in that mode.
    ///
    /// A receipt file that fails to parse is a typed error naming the file,
    /// never silently skipped. A receipt whose embedded `envelope_digest`
    /// disagrees with its filename is a linkage error naming the file.
    pub fn wait_for_receipts(
        &self,
        expected_digests: &[String],
        poll_interval_ms: u64,
        timeout_ms: u64,
    ) -> Result<Vec<ReceiptFile>, DriverError> {
        let start = Instant::now();
        let interval = Duration::from_millis(poll_interval_ms.max(1));

        loop {
            let mut collected: Vec<ReceiptFile> = Vec::new();
            for digest in expected_digests {
                let path = self.path_for(digest);
                if path.exists() {
                    // A present-but-unparseable receipt is a typed error
                    // naming the file — never silently skipped.
                    collected.push(self.read_receipt(&path, digest)?);
                }
            }
            if collected.len() == expected_digests.len() {
                return Ok(collected);
            }
            if timeout_ms > 0 && start.elapsed() >= Duration::from_millis(timeout_ms) {
                return Ok(collected);
            }
            std::thread::sleep(interval);
        }
    }

    /// Read and validate one receipt file. The embedded `envelope_digest`
    /// must match the filename's digest (linkage check).
    fn read_receipt(&self, path: &Path, expected_digest: &str) -> Result<ReceiptFile, DriverError> {
        let bytes = std::fs::read(path).map_err(|e| {
            DriverError::Backend(format!("cannot read receipt {}: {e}", path.display()))
        })?;
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
        let got = ib
            .wait_for_receipts(&[digest.to_string()], 10, 5_000)
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].envelope_digest, digest);
        assert_eq!(got[0].body["verdict"], "pass");
        assert!(got[0].file_digest.starts_with("sha256:"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn inbox_timeout_returns_what_arrived() {
        let d = tmpdir("timeout");
        let ib = Inbox::new(&d.join("inbox")).unwrap();
        let got = ib
            .wait_for_receipts(&["sha256:never".to_string()], 10, 120)
            .unwrap();
        assert!(got.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unparseable_receipt_is_typed_error_naming_file() {
        let d = tmpdir("poison");
        let ib = Inbox::new(&d.join("inbox")).unwrap();
        let digest = "sha256:bad1";
        std::fs::write(ib.path_for(digest), b"{not json").unwrap();
        let err = ib
            .wait_for_receipts(&[digest.to_string()], 10, 5_000)
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("receipt") && msg.contains(".receipt.json"), "{msg}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn linkage_mismatch_is_rejected() {
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
        let err = ib
            .wait_for_receipts(&[digest.to_string()], 10, 5_000)
            .unwrap_err();
        assert!(format!("{err}").contains("linkage"), "{err}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
