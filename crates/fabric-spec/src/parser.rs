//! T011 — INTERFACE CONTRACT (frozen by orchestrator).
//!
//! Parse a versioned master specification into typed requirements while
//! preserving source text and stable source locations.
//!
//! Worker S1: replace the `todo!()` body with a production implementation.
//! Public signatures are frozen; add private helpers freely.

use fabric_types::{Result, SpecId};
use serde::{Deserialize, Serialize};

/// Stable source location: 1-based inclusive line range in the spec text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceAnchor {
    pub line_start: u32,
    pub line_end: u32,
}

/// One requirement extracted from the master spec: original text plus anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    pub anchor: SourceAnchor,
    pub text: String,
}

/// A parsed master specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedSpec {
    pub revision: SpecId,
    /// sha256 of the exact source text parsed.
    pub source_digest: String,
    pub requirements: Vec<Requirement>,
}

/// Requirement marker grammar (deterministic, documented here):
///
/// - The text is split into blocks separated by blank lines, where a blank
///   line is empty or whitespace-only. Blank separators belong to no block.
/// - A block is a requirement iff its first line, after trimming leading
///   whitespace, starts with one of: `REQ:`, `REQ-`, `- id:`, `REQUIREMENT:`.
///   All other blocks are ignored (prose, headings, code fences).
/// - Every requirement keeps its verbatim block text: internal formatting is
///   preserved; only trailing whitespace/newlines at the very end of the
///   block are stripped.
/// - Anchors are 1-based inclusive line numbers covering the block's lines in
///   the original text; blank separator lines are counted for numbering but
///   never included in an anchor.
/// - `source_digest` is `sha256:` + lowercase hex of SHA-256 over the exact
///   input bytes.
/// - Empty or whitespace-only input yields an empty requirement list, never
///   an error.
pub fn parse_spec(text: &str, revision: SpecId) -> Result<ParsedSpec> {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for b in digest {
        hex.push_str(&format!("{:02x}", b));
    }
    let source_digest = format!("sha256:{}", hex);

    let mut requirements = Vec::new();
    if text.trim().is_empty() {
        return Ok(ParsedSpec {
            revision,
            source_digest,
            requirements,
        });
    }

    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim().is_empty() {
            i += 1;
            continue;
        }
        let start = i;
        while i < lines.len() && !lines[i].trim().is_empty() {
            i += 1;
        }
        let end = i - 1; // 0-based inclusive; i > start always here
        if is_requirement_block(lines[start]) {
            let block_text = lines[start..=end].join("\n");
            requirements.push(Requirement {
                anchor: SourceAnchor {
                    line_start: (start + 1) as u32,
                    line_end: (end + 1) as u32,
                },
                text: block_text.trim_end().to_string(),
            });
        }
    }

    Ok(ParsedSpec {
        revision,
        source_digest,
        requirements,
    })
}

/// True when the block's first line opens a requirement block.
fn is_requirement_block(first_line: &str) -> bool {
    let trimmed = first_line.trim_start();
    ["REQ:", "REQ-", "- id:", "REQUIREMENT:"]
        .iter()
        .any(|marker| trimmed.starts_with(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rev() -> SpecId {
        SpecId::new("spec:test").unwrap()
    }

    #[test]
    fn empty_input_yields_no_requirements() {
        for empty in ["", "   ", "\n\n  \n"] {
            let spec = parse_spec(empty, rev()).unwrap();
            assert!(spec.requirements.is_empty());
            assert!(spec.source_digest.starts_with("sha256:"));
            assert_eq!(spec.source_digest.len(), 7 + 64);
        }
    }

    #[test]
    fn extracts_marked_blocks_and_ignores_prose() {
        let text = "# Heading\n\nSome prose here.\n\nREQ: the system must boot\n\n- id: T002\n  objective: second thing\n";
        let spec = parse_spec(text, rev()).unwrap();
        assert_eq!(spec.requirements.len(), 2);
        assert_eq!(spec.requirements[0].text, "REQ: the system must boot");
        assert!(spec.requirements[1].text.starts_with("- id: T002"));
    }

    #[test]
    fn anchors_are_stable_one_based_inclusive() {
        let text = "prose\n\nREQ: first\nline two\n\n\nREQ- second\n";
        let spec = parse_spec(text, rev()).unwrap();
        assert_eq!(spec.requirements.len(), 2);
        assert_eq!(
            spec.requirements[0].anchor,
            SourceAnchor {
                line_start: 3,
                line_end: 4
            }
        );
        assert_eq!(
            spec.requirements[1].anchor,
            SourceAnchor {
                line_start: 7,
                line_end: 7
            }
        );
    }

    #[test]
    fn source_digest_is_deterministic_and_content_sensitive() {
        let a = parse_spec("REQ: x\n", rev()).unwrap();
        let b = parse_spec("REQ: x\n", rev()).unwrap();
        let c = parse_spec("REQ: y\n", rev()).unwrap();
        assert_eq!(a.source_digest, b.source_digest);
        assert_ne!(a.source_digest, c.source_digest);
    }

    #[test]
    fn marker_must_be_at_block_start() {
        // marker mid-block does not open a new requirement
        let text = "REQ: first\nmentions REQ: inside\n";
        let spec = parse_spec(text, rev()).unwrap();
        assert_eq!(spec.requirements.len(), 1);
        // indented marker still counts (leading whitespace trimmed)
        let text2 = "   REQUIREMENT: indented\n";
        let spec2 = parse_spec(text2, rev()).unwrap();
        assert_eq!(spec2.requirements.len(), 1);
    }

    #[test]
    fn verbatim_text_preserved() {
        let text = "REQ: keep  internal   spacing\n\tand\ttabs\n";
        let spec = parse_spec(text, rev()).unwrap();
        assert_eq!(
            spec.requirements[0].text,
            "REQ: keep  internal   spacing\n\tand\ttabs"
        );
    }

    #[test]
    fn round_trips_through_json() {
        let spec = parse_spec("REQ: x\n\n- id: T1\n", rev()).unwrap();
        let json = serde_json::to_string(&spec).unwrap();
        let back: ParsedSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, back);
    }
}
