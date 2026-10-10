//! T085 — per-attempt prompt storage.
//!
//! Stores the exact rendered prompt bytes for every attempt, keyed by
//! `(task_id, attempt)`, together with the task contract version, the shared
//! contract version, and the endpoint identity. Retrieval returns
//! byte-identical prompts: the digest is recomputed on every read and any
//! mismatch is a [`FabricError::Conflict`], never a silent wrong answer.
//!
//! Layout: `<dir>/<task_id>-a<attempt>/prompt.bin` (exact bytes) plus
//! `meta.json` (digest, versions, endpoint). Writes go through temp-file +
//! fsync + atomic rename; the meta file is written after the prompt bytes so
//! a crash can never leave metadata pointing at missing bytes.

use crate::receipts::sha256_prefixed;
use fabric_types::{EndpointId, FabricError, Result, TaskId};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const PROMPT_FILE: &str = "prompt.bin";
const META_FILE: &str = "meta.json";

/// The stored record for one attempt's prompt. The bytes themselves live in
/// `prompt.bin`; this struct is what `meta.json` carries plus the verified
/// digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptRecord {
    pub task_id: TaskId,
    pub attempt: u32,
    /// `sha256:` + hex of the exact stored prompt bytes.
    pub prompt_digest: String,
    pub prompt_bytes_len: u64,
    pub task_contract_version: String,
    /// Carried verbatim. This crate does not define the semantics of the
    /// shared contract version (no task defines it); it is recorded so a
    /// future spec can interpret it. Never invented: callers supply it.
    pub shared_contract_version: String,
    pub endpoint: EndpointId,
}

/// Content-addressed-by-attempt prompt store.
pub struct PromptStore {
    dir: PathBuf,
}

impl PromptStore {
    /// Open (creating) the store in `dir`.
    pub fn open(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir).map_err(|e| io_err(e, "create prompt store dir"))?;
        Ok(PromptStore {
            dir: dir.to_path_buf(),
        })
    }

    fn attempt_dir(&self, task_id: &TaskId, attempt: u32) -> PathBuf {
        // TaskId charset is [A-Za-z0-9-_.:], filesystem-safe on this platform.
        self.dir.join(format!("{}-a{}", task_id.as_str(), attempt))
    }

    /// Store a prompt for an attempt. Idempotent for identical bytes (returns
    /// the existing record); storing *different* bytes under an existing
    /// attempt key is a [`FabricError::Conflict`].
    pub fn store_prompt(
        &self,
        task_id: &TaskId,
        attempt: u32,
        prompt: &[u8],
        task_contract_version: &str,
        shared_contract_version: &str,
        endpoint: &EndpointId,
    ) -> Result<PromptRecord> {
        let digest = sha256_prefixed(prompt);
        let dir = self.attempt_dir(task_id, attempt);
        let meta_path = dir.join(META_FILE);
        if meta_path.exists() {
            let existing = self.read_meta(&meta_path)?;
            if existing.prompt_digest == digest {
                return Ok(existing);
            }
            return Err(FabricError::Conflict(format!(
                "attempt {}#{} already has different prompt bytes",
                task_id.as_str(),
                attempt
            )));
        }
        fs::create_dir_all(&dir).map_err(|e| io_err(e, "create attempt dir"))?;
        // Prompt bytes first, then metadata: a crash between the two leaves
        // an attempt dir with no meta, which readers treat as absent.
        write_file_durable(&dir.join(PROMPT_FILE), prompt)?;
        let record = PromptRecord {
            task_id: task_id.clone(),
            attempt,
            prompt_digest: digest,
            prompt_bytes_len: prompt.len() as u64,
            task_contract_version: task_contract_version.to_string(),
            shared_contract_version: shared_contract_version.to_string(),
            endpoint: endpoint.clone(),
        };
        let meta_bytes =
            serde_json::to_vec_pretty(&record).expect("meta serialization cannot fail");
        write_file_durable(&meta_path, &meta_bytes)?;
        Ok(record)
    }

    /// Retrieve a prompt record, integrity-checked: the digest is recomputed
    /// from `prompt.bin` and must match `meta.json`. Mismatch is a Conflict.
    pub fn get_prompt(&self, task_id: &TaskId, attempt: u32) -> Result<PromptRecord> {
        let dir = self.attempt_dir(task_id, attempt);
        let meta_path = dir.join(META_FILE);
        if !meta_path.exists() {
            return Err(FabricError::InvalidInput {
                field: "attempt",
                reason: "no prompt stored for this attempt".to_string(),
            });
        }
        let record = self.read_meta(&meta_path)?;
        let bytes = self.get_prompt_bytes(task_id, attempt)?;
        let actual = sha256_prefixed(&bytes);
        if actual != record.prompt_digest {
            return Err(FabricError::Conflict(format!(
                "prompt digest mismatch for {}#{}: stored {} != actual {}",
                task_id.as_str(),
                attempt,
                record.prompt_digest,
                actual
            )));
        }
        if bytes.len() as u64 != record.prompt_bytes_len {
            return Err(FabricError::Conflict(format!(
                "prompt length mismatch for {}#{}",
                task_id.as_str(),
                attempt
            )));
        }
        Ok(record)
    }

    /// Retrieve the verified prompt bytes for an attempt. Integrity is
    /// checked via [`PromptStore::get_prompt`]; bytes are never returned
    /// without verification.
    pub fn get_prompt_bytes(&self, task_id: &TaskId, attempt: u32) -> Result<Vec<u8>> {
        let path = self.attempt_dir(task_id, attempt).join(PROMPT_FILE);
        fs::read(&path).map_err(|e| io_err(e, "read prompt bytes"))
    }

    fn read_meta(&self, path: &Path) -> Result<PromptRecord> {
        let bytes = fs::read(path).map_err(|e| io_err(e, "read prompt meta"))?;
        serde_json::from_slice(&bytes).map_err(|_| {
            FabricError::MalformedOutput(format!("prompt meta does not parse: {}", path.display()))
        })
    }
}

fn io_err(e: std::io::Error, what: &'static str) -> FabricError {
    // The frozen T010 error enum has no storage variant; TransportFailure is
    // the closest typed carrier for local IO failure. Documented, not hidden.
    FabricError::TransportFailure(format!("{}: {}", what, e))
}

/// Write bytes to `path` via temp file + fsync + atomic rename.
fn write_file_durable(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| io_err(e, "open temp file"))?;
        f.write_all(bytes).map_err(|e| io_err(e, "write temp file"))?;
        f.flush().map_err(|e| io_err(e, "flush temp file"))?;
        f.sync_all().map_err(|e| io_err(e, "fsync temp file"))?;
    }
    fs::rename(&tmp, path).map_err(|e| io_err(e, "rename into place"))?;
    // Best effort: fsync the parent directory so the rename itself is durable.
    if let Ok(dir) = File::open(path.parent().unwrap_or_else(|| Path::new("."))) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn scratch_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rec3-prompts-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn tid(s: &str) -> TaskId {
        TaskId::new(s).unwrap()
    }
    fn eid(s: &str) -> EndpointId {
        EndpointId::new(s).unwrap()
    }

    #[test]
    fn roundtrip_returns_byte_identical_prompt() {
        let dir = scratch_dir();
        let store = PromptStore::open(&dir).unwrap();
        // Deliberately non-UTF8 bytes: the store must not touch the content.
        let prompt: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
        let rec = store
            .store_prompt(&tid("t-p-1"), 1, &prompt, "cv-task-9", "cv-shared-3", &eid("ep-1"))
            .unwrap();
        assert_eq!(rec.prompt_bytes_len, 4096);
        assert!(rec.prompt_digest.starts_with("sha256:"));
        assert_eq!(rec.task_contract_version, "cv-task-9");
        assert_eq!(rec.shared_contract_version, "cv-shared-3");
        assert_eq!(rec.endpoint.as_str(), "ep-1");
        let back = store.get_prompt(&tid("t-p-1"), 1).unwrap();
        assert_eq!(back, rec);
        assert_eq!(store.get_prompt_bytes(&tid("t-p-1"), 1).unwrap(), prompt);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_detects_mutated_prompt_bytes() {
        let dir = scratch_dir();
        let store = PromptStore::open(&dir).unwrap();
        store
            .store_prompt(&tid("t-p-2"), 1, b"original bytes", "v1", "s1", &eid("ep-1"))
            .unwrap();
        // Tamper with the bytes after the fact.
        fs::write(dir.join("t-p-2-a1").join(PROMPT_FILE), b"tampered bytes").unwrap();
        let err = store.get_prompt(&tid("t-p-2"), 1).unwrap_err();
        assert!(matches!(err, FabricError::Conflict(_)), "got {:?}", err);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_detects_mutated_meta() {
        let dir = scratch_dir();
        let store = PromptStore::open(&dir).unwrap();
        store
            .store_prompt(&tid("t-p-3"), 1, b"bytes", "v1", "s1", &eid("ep-1"))
            .unwrap();
        let meta_path = dir.join("t-p-3-a1").join(META_FILE);
        let mut meta: serde_json::Value =
            serde_json::from_slice(&fs::read(&meta_path).unwrap()).unwrap();
        meta["prompt_digest"] = serde_json::Value::String("sha256:deadbeef".into());
        fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();
        let err = store.get_prompt(&tid("t-p-3"), 1).unwrap_err();
        assert!(matches!(err, FabricError::Conflict(_)), "got {:?}", err);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn store_is_idempotent_for_identical_bytes() {
        let dir = scratch_dir();
        let store = PromptStore::open(&dir).unwrap();
        let r1 = store
            .store_prompt(&tid("t-p-4"), 1, b"same", "v1", "s1", &eid("ep-1"))
            .unwrap();
        let r2 = store
            .store_prompt(&tid("t-p-4"), 1, b"same", "v1", "s1", &eid("ep-1"))
            .unwrap();
        assert_eq!(r1, r2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn different_bytes_same_attempt_is_conflict() {
        let dir = scratch_dir();
        let store = PromptStore::open(&dir).unwrap();
        store
            .store_prompt(&tid("t-p-5"), 1, b"first", "v1", "s1", &eid("ep-1"))
            .unwrap();
        let err = store
            .store_prompt(&tid("t-p-5"), 1, b"second", "v1", "s1", &eid("ep-1"))
            .unwrap_err();
        assert!(matches!(err, FabricError::Conflict(_)), "got {:?}", err);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_attempt_is_not_found() {
        let dir = scratch_dir();
        let store = PromptStore::open(&dir).unwrap();
        let err = store.get_prompt(&tid("t-p-6"), 7).unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }), "got {:?}", err);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_prompt_roundtrips() {
        let dir = scratch_dir();
        let store = PromptStore::open(&dir).unwrap();
        let rec = store
            .store_prompt(&tid("t-p-7"), 1, b"", "v1", "s1", &eid("ep-1"))
            .unwrap();
        assert_eq!(rec.prompt_bytes_len, 0);
        assert_eq!(store.get_prompt_bytes(&tid("t-p-7"), 1).unwrap(), b"");
        let _ = fs::remove_dir_all(&dir);
    }
}
