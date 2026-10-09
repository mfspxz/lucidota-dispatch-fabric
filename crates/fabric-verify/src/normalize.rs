//! T094 — test-output normalization.
//!
//! Normalizes supported test outputs into pass/fail/skip counts plus evidence
//! references, without inventing data:
//!
//! - Cargo libtest lines: `test <name> ... ok` / `... FAILED` / `... ignored`.
//! - Generic line protocol: `PASS: <label>`, `FAIL: <label>`, `SKIP: <label>`.
//!
//! Unrecognized lines are preserved **verbatim** in `unparsed` — never
//! dropped, never reinterpreted as results. In particular, libtest summary
//! lines (`test result: ok. 5 passed; ...`) are *not* counted; only
//! per-test lines count, so a combined stream can never double-count.

use serde::{Deserialize, Serialize};

/// Outcome of one recognized test line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TestOutcome {
    Pass,
    Fail,
    Skip,
}

/// One recognized test result with its evidence reference (test name/label).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub name: String,
    pub outcome: TestOutcome,
}

/// Normalized test results. Counts are derived only from recognized lines.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedResults {
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
    /// Lines that matched no known format, preserved verbatim.
    pub unparsed: Vec<String>,
    /// Evidence references from recognized lines, in stream order.
    pub evidence_refs: Vec<EvidenceRef>,
}

/// Parse one libtest per-test line: `test <name> ... ok|FAILED|ignored`.
/// Returns None for anything else (including `test result:` summaries).
fn parse_libtest(line: &str) -> Option<(&str, TestOutcome)> {
    let rest = line.strip_prefix("test ")?;
    // Summary lines start with "test result:" — never count those.
    if rest.starts_with("result:") {
        return None;
    }
    let (name, status) = rest.split_once(" ... ")?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let outcome = match status.trim() {
        "ok" => TestOutcome::Pass,
        "FAILED" => TestOutcome::Fail,
        "ignored" => TestOutcome::Skip,
        _ => return None,
    };
    Some((name, outcome))
}

/// Parse one generic protocol line: `PASS: <label>` etc.
fn parse_generic(line: &str) -> Option<(&str, TestOutcome)> {
    let trimmed = line.trim_start();
    for (prefix, outcome) in [
        ("PASS:", TestOutcome::Pass),
        ("FAIL:", TestOutcome::Fail),
        ("SKIP:", TestOutcome::Skip),
    ] {
        if let Some(label) = trimmed.strip_prefix(prefix) {
            let label = label.trim();
            if label.is_empty() {
                return None; // a bare prefix invents nothing
            }
            return Some((label, outcome));
        }
    }
    None
}

/// Normalize test output text into counts and evidence references.
pub fn normalize(text: &str) -> NormalizedResults {
    let mut out = NormalizedResults::default();
    for line in text.lines() {
        let parsed = parse_libtest(line).or_else(|| parse_generic(line));
        match parsed {
            Some((name, outcome)) => {
                match outcome {
                    TestOutcome::Pass => out.passed += 1,
                    TestOutcome::Fail => out.failed += 1,
                    TestOutcome::Skip => out.skipped += 1,
                }
                out.evidence_refs.push(EvidenceRef {
                    name: name.to_string(),
                    outcome,
                });
            }
            None => out.unparsed.push(line.to_string()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libtest_lines_counted() {
        let text = "\
test alpha ... ok
test beta ... FAILED
test gamma ... ignored
";
        let r = normalize(text);
        assert_eq!((r.passed, r.failed, r.skipped), (1, 1, 1));
        assert!(r.unparsed.is_empty());
        let names: Vec<&str> = r.evidence_refs.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn summary_lines_are_not_counted() {
        let text = "\
test alpha ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
";
        let r = normalize(text);
        assert_eq!((r.passed, r.failed, r.skipped), (1, 0, 0));
        assert_eq!(r.unparsed.len(), 1);
        assert!(r.unparsed[0].starts_with("test result:"));
    }

    #[test]
    fn generic_protocol_lines_counted() {
        let text = "PASS: schema-check\nFAIL: lint\nSKIP: fuzz\n";
        let r = normalize(text);
        assert_eq!((r.passed, r.failed, r.skipped), (1, 1, 1));
        assert!(r.unparsed.is_empty());
    }

    #[test]
    fn unrecognized_lines_preserved_verbatim() {
        let text = "   some random log line   \ntest broken ... ???\nPASS:\n";
        let r = normalize(text);
        assert_eq!((r.passed, r.failed, r.skipped), (0, 0, 0));
        // Verbatim: leading/trailing whitespace kept, nothing reinterpreted.
        assert_eq!(
            r.unparsed,
            vec![
                "   some random log line   ".to_string(),
                "test broken ... ???".to_string(),
                "PASS:".to_string(),
            ]
        );
    }

    #[test]
    fn bare_prefix_invents_nothing() {
        let r = normalize("PASS:\nFAIL:   \n");
        assert_eq!((r.passed, r.failed, r.skipped), (0, 0, 0));
        assert_eq!(r.unparsed.len(), 2);
    }

    #[test]
    fn empty_input_normalizes_to_zero() {
        let r = normalize("");
        assert_eq!((r.passed, r.failed, r.skipped), (0, 0, 0));
        assert!(r.unparsed.is_empty());
        assert!(r.evidence_refs.is_empty());
    }

    #[test]
    fn mixed_stream_keeps_order() {
        let text = "PASS: a\ntest b ... FAILED\nnoise\ntest c ... ok\n";
        let r = normalize(text);
        assert_eq!((r.passed, r.failed, r.skipped), (2, 1, 0));
        assert_eq!(r.unparsed, vec!["noise".to_string()]);
        let outcomes: Vec<TestOutcome> =
            r.evidence_refs.iter().map(|e| e.outcome).collect();
        assert_eq!(
            outcomes,
            vec![TestOutcome::Pass, TestOutcome::Fail, TestOutcome::Pass]
        );
    }
}
