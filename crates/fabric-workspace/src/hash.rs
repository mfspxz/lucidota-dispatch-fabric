//! T075: hash patches and artifacts; detect post-capture mutation.
//!
//! Capture model is store-then-verify: [`seal_file`] records the sha256,
//! size, and capture time of an artifact; [`verify_sealed`] re-hashes later
//! and reports any post-capture mutation as a [`FabricError::Conflict`].
//! Hashing never mutates the artifact.

use fabric_types::{FabricError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// A sha256 content hash.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentHash {
    pub algo: String,
    pub hex: String,
}

impl ContentHash {
    /// Parse `sha256:<hex>`.
    pub fn parse(s: &str) -> Result<Self> {
        let (algo, hex) = s.split_once(':').ok_or(FabricError::InvalidInput {
            field: "content_hash",
            reason: "must be algo:hex".to_string(),
        })?;
        if algo != "sha256" {
            return Err(FabricError::InvalidInput {
                field: "content_hash",
                reason: "only sha256 is supported".to_string(),
            });
        }
        if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(FabricError::InvalidInput {
                field: "content_hash",
                reason: "hex part must be non-empty hex".to_string(),
            });
        }
        Ok(Self {
            algo: algo.to_string(),
            hex: hex.to_ascii_lowercase(),
        })
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.algo, self.hex)
    }
}

fn hex(bytes: &[u8]) -> String {
    const LUT: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(LUT[(b >> 4) as usize] as char);
        s.push(LUT[(b & 0xf) as usize] as char);
    }
    s
}

/// sha256 of in-memory bytes.
pub fn hash_bytes(data: &[u8]) -> ContentHash {
    let mut h = Sha256::new();
    h.update(data);
    ContentHash {
        algo: "sha256".to_string(),
        hex: hex(&h.finalize()),
    }
}

/// sha256 of patch text (normalized to `\n` line endings first, so the same
/// logical patch hashes identically regardless of platform).
pub fn hash_patch(patch_text: &str) -> ContentHash {
    let normalized = patch_text.replace("\r\n", "\n");
    hash_bytes(normalized.as_bytes())
}

/// sha256 of a file's contents.
pub fn hash_file(path: &Path) -> Result<ContentHash> {
    let data = std::fs::read(path).map_err(|e| FabricError::InvalidInput {
        field: "path",
        reason: format!("cannot read {}: {e}", path.display()),
    })?;
    Ok(hash_bytes(&data))
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Integrity snapshot of an artifact at capture time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedArtifact {
    /// Path as sealed (diagnostic only; [`verify_sealed`] takes the live path).
    pub path: PathBuf,
    pub hash: ContentHash,
    pub size_bytes: u64,
    pub captured_at_millis: u64,
}

/// Seal a file: record hash + size + capture time without modifying it.
pub fn seal_file(path: &Path) -> Result<SealedArtifact> {
    let meta = std::fs::metadata(path).map_err(|e| FabricError::InvalidInput {
        field: "path",
        reason: format!("cannot stat {}: {e}", path.display()),
    })?;
    let hash = hash_file(path)?;
    Ok(SealedArtifact {
        path: path.to_path_buf(),
        hash,
        size_bytes: meta.len(),
        captured_at_millis: now_millis(),
    })
}

/// Re-hash `path` and compare against the seal. Any difference — size or
/// content — is a post-capture mutation and is reported as a conflict.
pub fn verify_sealed(path: &Path, sealed: &SealedArtifact) -> Result<()> {
    let meta = std::fs::metadata(path).map_err(|e| FabricError::InvalidInput {
        field: "path",
        reason: format!("cannot stat {}: {e}", path.display()),
    })?;
    if meta.len() != sealed.size_bytes {
        return Err(FabricError::Conflict(format!(
            "post-capture mutation detected (size changed): {} sealed={}b live={}b",
            path.display(),
            sealed.size_bytes,
            meta.len()
        )));
    }
    let live = hash_file(path)?;
    if live != sealed.hash {
        return Err(FabricError::Conflict(format!(
            "post-capture mutation detected (content changed): {} sealed={} live={}",
            path.display(),
            sealed.hash,
            live
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "fabric-ws-hash-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst),
            tag
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn sha256_known_vector() {
        // sha256("abc")
        let h = hash_bytes(b"abc");
        assert_eq!(
            h.to_string(),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn parse_round_trip_and_rejects() {
        let h = hash_bytes(b"x");
        let back = ContentHash::parse(&h.to_string()).unwrap();
        assert_eq!(h, back);
        assert!(ContentHash::parse("md5:abcd").is_err());
        assert!(ContentHash::parse("sha256:xyz").is_err());
        assert!(ContentHash::parse("no-colon").is_err());
    }

    #[test]
    fn patch_hash_normalizes_crlf() {
        assert_eq!(hash_patch("a\r\nb\r\n"), hash_patch("a\nb\n"));
    }

    #[test]
    fn seal_then_verify_ok() {
        let d = tmp("ok");
        let f = d.join("a.bin");
        std::fs::write(&f, b"hello").unwrap();
        let sealed = seal_file(&f).unwrap();
        assert_eq!(sealed.size_bytes, 5);
        verify_sealed(&f, &sealed).unwrap();
    }

    #[test]
    fn detects_content_mutation() {
        let d = tmp("mut");
        let f = d.join("a.bin");
        std::fs::write(&f, b"hello").unwrap();
        let sealed = seal_file(&f).unwrap();
        std::fs::write(&f, b"HELLO").unwrap(); // same length, different bytes
        let err = verify_sealed(&f, &sealed).unwrap_err();
        assert!(matches!(err, FabricError::Conflict(_)), "{err:?}");
        assert!(err.to_string().contains("content changed"));
    }

    #[test]
    fn detects_size_mutation() {
        let d = tmp("size");
        let f = d.join("a.bin");
        std::fs::write(&f, b"hello").unwrap();
        let sealed = seal_file(&f).unwrap();
        std::fs::write(&f, b"hello world").unwrap();
        let err = verify_sealed(&f, &sealed).unwrap_err();
        assert!(err.to_string().contains("size changed"));
    }

    #[test]
    fn seal_missing_file_errors() {
        let d = tmp("missing");
        assert!(seal_file(&d.join("nope")).is_err());
    }
}
