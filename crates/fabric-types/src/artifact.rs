//! T006: content-addressed ArtifactRef.

use crate::error::{FabricError, Result};
use serde::{Deserialize, Serialize};

/// T006: reference to an immutable artifact by content digest.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArtifactRef {
    /// `algo:hex`, e.g. `sha256:9f86d081...`.
    pub digest: String,
    pub media_type: String,
    pub size_bytes: u64,
    /// Where the artifact came from (worker id, URL, tool name...).
    pub provenance: String,
}

impl ArtifactRef {
    pub fn new(
        digest: impl Into<String>,
        media_type: impl Into<String>,
        size_bytes: u64,
        provenance: impl Into<String>,
    ) -> Result<Self> {
        let digest = digest.into();
        let (algo, hex) = digest.split_once(':').ok_or(FabricError::InvalidInput {
            field: "digest",
            reason: "must be algo:hex".to_string(),
        })?;
        if algo.is_empty()
            || !algo
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(FabricError::InvalidInput {
                field: "digest",
                reason: "algorithm part is invalid".to_string(),
            });
        }
        if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(FabricError::InvalidInput {
                field: "digest",
                reason: "hex part must be non-empty hex".to_string(),
            });
        }
        let media_type = media_type.into();
        if media_type.is_empty() || media_type.len() > 128 {
            return Err(FabricError::InvalidInput {
                field: "media_type",
                reason: "must be 1..=128 chars".to_string(),
            });
        }
        Ok(Self {
            digest,
            media_type,
            size_bytes,
            provenance: provenance.into(),
        })
    }

    /// The hash algorithm portion of the digest.
    pub fn algorithm(&self) -> &str {
        self.digest.split(':').next().unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_ref() {
        let r = ArtifactRef::new("sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08", "application/x-patch", 42, "worker-a").unwrap();
        assert_eq!(r.algorithm(), "sha256");
    }

    #[test]
    fn rejects_bad_digests() {
        assert!(ArtifactRef::new("no-colon", "t", 0, "p").is_err());
        assert!(ArtifactRef::new("sha256:xyz", "t", 0, "p").is_err());
        assert!(ArtifactRef::new(":abc", "t", 0, "p").is_err());
        assert!(ArtifactRef::new("sha256:", "t", 0, "p").is_err());
    }

    #[test]
    fn round_trips() {
        let r = ArtifactRef::new("blake3:abcd", "text/plain", 7, "x").unwrap();
        let j = serde_json::to_string(&r).unwrap();
        let back: ArtifactRef = serde_json::from_str(&j).unwrap();
        assert_eq!(r, back);
    }
}
