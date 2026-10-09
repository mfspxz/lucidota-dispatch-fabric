//! T088 — credential and sensitive-pattern redaction.
//!
//! Redacts credentials and configured sensitive patterns from text while
//! preserving reproducibility metadata. Redaction replaces each match with
//! `[REDACTED:<pattern-name>]`; the redaction log records WHAT was redacted
//! (pattern names and counts) and never the redacted values.
//!
//! Patterns are substring matchers or a small built-in regex-lite
//! (literals, `.`, `*`, `+`, `?`, `{m}`, `{m,}`, `{m,n}`, `[...]`,
//! `(a|b)` groups with alternation, `\d \w \s`, `^`, `$`, optional
//! case-insensitivity). No new dependencies.

use fabric_types::{FabricError, Result};
use serde::{Deserialize, Serialize};

/// A compiled regex-lite pattern.
#[derive(Debug, Clone)]
pub struct RegexLite {
    source: String,
    nodes: Vec<QuantNode>,
    anchored_start: bool,
    anchored_end: bool,
    case_insensitive: bool,
}

#[derive(Debug, Clone)]
struct QuantNode {
    kind: NodeKind,
    min: u32,
    /// None = unbounded.
    max: Option<u32>,
}

#[derive(Debug, Clone)]
enum NodeKind {
    Literal(char),
    Dot,
    Class { ranges: Vec<(char, char)>, negated: bool },
    /// `(a|b|c)`: alternation branches, each a node sequence.
    Group(Vec<Vec<QuantNode>>),
}

impl RegexLite {
    /// Compile a regex-lite pattern. Rejects empty patterns, dangling
    /// escapes, unterminated classes, and bad `{m,n}` bounds.
    pub fn compile(source: &str) -> Result<Self> {
        let mut p = Parser {
            chars: source.chars().collect(),
            pos: 0,
        };
        let mut nodes = Vec::new();
        let mut anchored_start = false;
        let mut anchored_end = false;
        if p.peek() == Some('^') {
            p.pos += 1;
            anchored_start = true;
        }
        while p.pos < p.chars.len() {
            let c = p.chars[p.pos];
            if c == '$' && p.pos + 1 == p.chars.len() {
                anchored_end = true;
                p.pos += 1;
                break;
            }
            let kind = p.parse_atom()?;
            let (min, max) = p.parse_quantifier();
            nodes.push(QuantNode { kind, min, max });
        }
        if nodes.is_empty() {
            return Err(FabricError::InvalidInput {
                field: "pattern",
                reason: "pattern must not be empty".to_string(),
            });
        }
        Ok(Self {
            source: source.to_string(),
            nodes,
            anchored_start,
            anchored_end,
            case_insensitive: false,
        })
    }

    /// Compile with case-insensitive matching. Case folding applies to
    /// literal characters only, not to character-class ranges (write both
    /// cases explicitly in classes when needed).
    pub fn compile_insensitive(source: &str) -> Result<Self> {
        let mut r = Self::compile(source)?;
        r.case_insensitive = true;
        Ok(r)
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// Leftmost match as byte offsets, or None. Zero-length matches are
    /// never returned.
    pub fn find_leftmost(&self, text: &str) -> Option<(usize, usize)> {
        let chars: Vec<char> = text.chars().collect();
        let byte_offsets: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        let starts: Vec<usize> = if self.anchored_start {
            vec![0]
        } else {
            (0..=chars.len()).collect()
        };
        for s in starts {
            if let Some(e) = self.match_at(&chars, s) {
                if e > s && (!self.anchored_end || e == chars.len()) {
                    let bs = *byte_offsets.get(s).unwrap_or(&text.len());
                    let be = byte_offsets.get(e).copied().unwrap_or(text.len());
                    return Some((bs, be));
                }
            }
        }
        None
    }

    /// All non-overlapping matches, left to right, as byte offsets.
    pub fn find_all(&self, text: &str) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut rest = text;
        let mut base = 0usize;
        while !rest.is_empty() || out.is_empty() {
            match self.find_leftmost(rest) {
                Some((s, e)) => {
                    out.push((base + s, base + e));
                    // Advance past the match; s==0 always for a fresh
                    // leftmost search on `rest` unless anchored... anchored
                    // patterns are handled by slicing only after a match.
                    let adv = e.max(1);
                    base += adv;
                    rest = &rest[adv.min(rest.len())..];
                    if self.anchored_start {
                        break;
                    }
                }
                None => break,
            }
            if self.anchored_start {
                break;
            }
        }
        out
    }

    /// Greedy match of all nodes starting at char position `pos`; returns
    /// the end char position of the longest match, or None.
    fn match_at(&self, chars: &[char], pos: usize) -> Option<usize> {
        self.match_seq(&self.nodes, chars, pos).into_iter().max()
    }

    /// All end positions reachable by matching a node sequence at `pos`.
    fn match_seq(&self, nodes: &[QuantNode], chars: &[char], pos: usize) -> Vec<usize> {
        let mut ends = vec![pos];
        for node in nodes {
            let mut next = Vec::new();
            for &p in &ends {
                next.extend(self.match_node(node, chars, p));
            }
            // Deduplicate to bound backtracking blowup.
            next.sort_unstable();
            next.dedup();
            if next.is_empty() {
                return Vec::new();
            }
            ends = next;
        }
        ends
    }

    /// All end positions reachable by matching one quantified node at `pos`.
    /// Greedy: longer repetitions are listed first.
    fn match_node(&self, node: &QuantNode, chars: &[char], pos: usize) -> Vec<usize> {
        if let NodeKind::Group(branches) = &node.kind {
            return self.match_group(node, branches, chars, pos);
        }
        let mut positions = vec![pos];
        let mut results = Vec::new();
        // Maximum repetitions we can attempt: bounded by remaining chars for
        // unbounded quantifiers (each repetition consumes >= 1 char).
        let max_rep = node
            .max
            .unwrap_or_else(|| (chars.len() - pos) as u32 + node.min);
        for rep in 0..=max_rep {
            if rep >= node.min {
                results.push(positions[rep as usize]);
            }
            if rep == max_rep {
                break;
            }
            let cur = positions[rep as usize];
            match self.match_one(&node.kind, chars, cur) {
                Some(nxt) => positions.push(nxt),
                None => break,
            }
        }
        results.reverse(); // greedy first
        results
    }

    /// Match a quantified group `(a|b)`: union the branches' end positions
    /// per repetition, then apply the quantifier.
    fn match_group(
        &self,
        node: &QuantNode,
        branches: &[Vec<QuantNode>],
        chars: &[char],
        pos: usize,
    ) -> Vec<usize> {
        let max_rep = node
            .max
            .unwrap_or_else(|| (chars.len() - pos) as u32 + node.min);
        let mut results = Vec::new();
        let mut current = vec![pos];
        for rep in 0..=max_rep {
            if rep >= node.min {
                results.extend(current.iter().copied());
            }
            if rep == max_rep {
                break;
            }
            let mut next = Vec::new();
            for &p in &current {
                for branch in branches {
                    next.extend(self.match_seq(branch, chars, p));
                }
            }
            next.sort_unstable();
            next.dedup();
            // No progress possible: stop rather than spin.
            if next.is_empty() || next == current {
                break;
            }
            current = next;
        }
        results.sort_unstable();
        results.dedup();
        results.reverse(); // greedy first
        results
    }

    fn match_one(&self, kind: &NodeKind, chars: &[char], pos: usize) -> Option<usize> {
        let c = *chars.get(pos)?;
        let ok = match kind {
            NodeKind::Literal(l) => {
                if self.case_insensitive {
                    c.to_lowercase().eq(l.to_lowercase())
                } else {
                    c == *l
                }
            }
            NodeKind::Dot => true,
            NodeKind::Class { ranges, negated } => {
                let hit = ranges.iter().any(|(lo, hi)| c >= *lo && c <= *hi);
                hit != *negated
            }
            // Unreachable: groups are intercepted in `match_node` and never
            // reach single-character matching.
            NodeKind::Group(_) => false,
        };
        ok.then_some(pos + 1)
    }
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn parse_atom(&mut self) -> Result<NodeKind> {
        let c = self.peek().ok_or(FabricError::InvalidInput {
            field: "pattern",
            reason: "unexpected end of pattern".to_string(),
        })?;
        self.pos += 1;
        match c {
            '.' => Ok(NodeKind::Dot),
            '[' => self.parse_class(),
            '(' => self.parse_group(),
            ')' => Err(FabricError::InvalidInput {
                field: "pattern",
                reason: "unbalanced ')'".to_string(),
            }),
            '\\' => {
                let e = self.peek().ok_or(FabricError::InvalidInput {
                    field: "pattern",
                    reason: "dangling escape".to_string(),
                })?;
                self.pos += 1;
                match e {
                    'd' => Ok(NodeKind::Class {
                        ranges: vec![('0', '9')],
                        negated: false,
                    }),
                    'w' => Ok(NodeKind::Class {
                        ranges: vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')],
                        negated: false,
                    }),
                    's' => Ok(NodeKind::Class {
                        ranges: vec![(' ', ' '), ('\t', '\t'), ('\n', '\n'), ('\r', '\r')],
                        negated: false,
                    }),
                    _ => Ok(NodeKind::Literal(e)),
                }
            }
            _ => Ok(NodeKind::Literal(c)),
        }
    }

    fn parse_class(&mut self) -> Result<NodeKind> {
        let negated = if self.peek() == Some('^') {
            self.pos += 1;
            true
        } else {
            false
        };
        let mut ranges = Vec::new();
        loop {
            let c = self.peek().ok_or(FabricError::InvalidInput {
                field: "pattern",
                reason: "unterminated character class".to_string(),
            })?;
            if c == ']' {
                self.pos += 1;
                break;
            }
            self.pos += 1;
            let lo = if c == '\\' {
                let e = self.peek().ok_or(FabricError::InvalidInput {
                    field: "pattern",
                    reason: "dangling escape in class".to_string(),
                })?;
                self.pos += 1;
                e
            } else {
                c
            };
            if self.peek() == Some('-') && self.chars.get(self.pos + 1) != Some(&']') {
                self.pos += 1; // consume '-'
                let hi = self.peek().ok_or(FabricError::InvalidInput {
                    field: "pattern",
                    reason: "dangling range in class".to_string(),
                })?;
                self.pos += 1;
                if hi < lo {
                    return Err(FabricError::InvalidInput {
                        field: "pattern",
                        reason: "reversed range in class".to_string(),
                    });
                }
                ranges.push((lo, hi));
            } else {
                ranges.push((lo, lo));
            }
        }
        if ranges.is_empty() {
            return Err(FabricError::InvalidInput {
                field: "pattern",
                reason: "empty character class".to_string(),
            });
        }
        Ok(NodeKind::Class { ranges, negated })
    }

    /// Parse `(branch|branch)` — called after consuming '('.
    fn parse_group(&mut self) -> Result<NodeKind> {
        let mut branches: Vec<Vec<QuantNode>> = Vec::new();
        let mut current: Vec<QuantNode> = Vec::new();
        loop {
            let c = self.peek().ok_or(FabricError::InvalidInput {
                field: "pattern",
                reason: "unterminated group".to_string(),
            })?;
            if c == ')' {
                self.pos += 1;
                break;
            }
            if c == '|' {
                self.pos += 1;
                branches.push(std::mem::take(&mut current));
                continue;
            }
            let kind = self.parse_atom()?;
            let (min, max) = self.parse_quantifier();
            current.push(QuantNode { kind, min, max });
        }
        branches.push(current);
        if branches.iter().all(|b| b.is_empty()) {
            return Err(FabricError::InvalidInput {
                field: "pattern",
                reason: "empty group".to_string(),
            });
        }
        Ok(NodeKind::Group(branches))
    }

    fn parse_quantifier(&mut self) -> (u32, Option<u32>) {
        match self.peek() {
            Some('*') => {
                self.pos += 1;
                (0, None)
            }
            Some('+') => {
                self.pos += 1;
                (1, None)
            }
            Some('?') => {
                self.pos += 1;
                (0, Some(1))
            }
            Some('{') => {
                let save = self.pos;
                match self.parse_braces() {
                    Some(q) => q,
                    None => {
                        // Not a valid quantifier: rewind and treat '{' as a
                        // literal (the atom parser will re-read it as such
                        // only if we don't consume it here — so report "no
                        // quantifier" and leave pos at '{').
                        self.pos = save;
                        (1, Some(1))
                    }
                }
            }
            _ => (1, Some(1)),
        }
    }

    fn parse_braces(&mut self) -> Option<(u32, Option<u32>)> {
        let start = self.pos;
        self.pos += 1; // consume '{'
        let m = self.parse_num()?;
        let (min, max) = if self.peek() == Some(',') {
            self.pos += 1;
            if self.peek() == Some('}') {
                (m, None)
            } else {
                let n = self.parse_num()?;
                if n < m {
                    return None;
                }
                (m, Some(n))
            }
        } else {
            (m, Some(m))
        };
        if self.peek() != Some('}') {
            self.pos = start; // not a quantifier; rewind, treat '{' literally
            return None;
        }
        self.pos += 1;
        Some((min, max))
    }

    fn parse_num(&mut self) -> Option<u32> {
        let start = self.pos;
        let mut n: u32 = 0;
        while let Some(c) = self.peek() {
            if !c.is_ascii_digit() {
                break;
            }
            n = n.checked_mul(10)?.checked_add(c as u32 - '0' as u32)?;
            self.pos += 1;
        }
        (self.pos > start).then_some(n)
    }
}

/// One configured redaction pattern.
#[derive(Debug, Clone)]
pub struct RedactPattern {
    name: String,
    matcher: PatternMatcher,
}

#[derive(Debug, Clone)]
enum PatternMatcher {
    Substring(String),
    Regex(RegexLite),
}

impl RedactPattern {
    /// Literal substring pattern.
    pub fn substring(name: impl Into<String>, needle: impl Into<String>) -> Result<Self> {
        let name = validate_name(name.into())?;
        let needle = needle.into();
        if needle.is_empty() {
            return Err(FabricError::InvalidInput {
                field: "needle",
                reason: "substring pattern must not be empty".to_string(),
            });
        }
        Ok(Self {
            name,
            matcher: PatternMatcher::Substring(needle),
        })
    }

    /// Regex-lite pattern (see module docs).
    pub fn regex(name: impl Into<String>, source: &str) -> Result<Self> {
        let name = validate_name(name.into())?;
        Ok(Self {
            name,
            matcher: PatternMatcher::Regex(RegexLite::compile(source)?),
        })
    }

    /// Case-insensitive regex-lite pattern.
    pub fn regex_insensitive(name: impl Into<String>, source: &str) -> Result<Self> {
        let name = validate_name(name.into())?;
        Ok(Self {
            name,
            matcher: PatternMatcher::Regex(RegexLite::compile_insensitive(source)?),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Non-overlapping match spans as byte offsets, left to right.
    fn find_all(&self, text: &str) -> Vec<(usize, usize)> {
        match &self.matcher {
            PatternMatcher::Substring(needle) => {
                let mut out = Vec::new();
                let mut from = 0usize;
                while let Some(i) = text[from..].find(needle.as_str()) {
                    let s = from + i;
                    out.push((s, s + needle.len()));
                    from = s + needle.len().max(1);
                }
                out
            }
            PatternMatcher::Regex(r) => r.find_all(text),
        }
    }
}

fn validate_name(name: String) -> Result<String> {
    if name.is_empty() || name.len() > 64 {
        return Err(FabricError::InvalidInput {
            field: "pattern_name",
            reason: "length must be 1..=64 characters".to_string(),
        });
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(FabricError::InvalidInput {
            field: "pattern_name",
            reason: "charset is [A-Za-z0-9_-]".to_string(),
        });
    }
    Ok(name)
}

/// One log entry: WHAT was redacted (pattern name) and how many times.
/// Values are never recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionEntry {
    pub pattern_name: String,
    pub count: u32,
}

/// The redaction log: pattern names and counts only, never values.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionLog {
    pub entries: Vec<RedactionEntry>,
    /// Length of the original text in bytes (reproducibility metadata).
    pub original_len: u64,
}

impl RedactionLog {
    /// Total redactions across all patterns.
    pub fn total(&self) -> u64 {
        self.entries.iter().map(|e| e.count as u64).sum()
    }
}

/// Outcome of redacting one text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionOutcome {
    pub redacted: String,
    pub log: RedactionLog,
}

/// Redacts credentials and configured sensitive patterns.
#[derive(Debug, Default)]
pub struct Redactor {
    patterns: Vec<RedactPattern>,
}

impl Redactor {
    pub fn new(patterns: Vec<RedactPattern>) -> Result<Self> {
        let mut seen = std::collections::HashSet::new();
        for p in &patterns {
            if !seen.insert(p.name.clone()) {
                return Err(FabricError::InvalidInput {
                    field: "patterns",
                    reason: format!("duplicate pattern name: {}", p.name),
                });
            }
        }
        Ok(Self { patterns })
    }

    /// Redactor with conservative credential defaults: bearer tokens and
    /// `key/secret/password/token` assignments. The value is redacted; the
    /// log records the pattern name and count.
    pub fn with_credential_defaults() -> Result<Self> {
        Self::new(vec![
            RedactPattern::regex("bearer-token", "[Bb]earer\\s+[A-Za-z0-9\\-._~+/=]{8,}")?,
            RedactPattern::regex_insensitive(
                "secret-assignment",
                "(api[_-]?key|secret|password|passwd|token)\\s*[:=]\\s*\\S+",
            )?,
        ])
    }

    pub fn patterns(&self) -> &[RedactPattern] {
        &self.patterns
    }

    /// Redact all configured patterns. Scanning is left-to-right; at each
    /// position the first pattern (in configuration order) that matches
    /// wins; matches never overlap. Deterministic for a fixed pattern list.
    pub fn redact(&self, text: &str) -> RedactionOutcome {
        // Collect candidate matches from every pattern, then resolve
        // overlaps deterministically: earliest start wins; ties go to the
        // earliest-configured pattern; then the longest match.
        let mut cands: Vec<(usize, usize, usize)> = Vec::new(); // (start, end, pattern_idx)
        for (idx, p) in self.patterns.iter().enumerate() {
            for (s, e) in p.find_all(text) {
                cands.push((s, e, idx));
            }
        }
        cands.sort_by(|a, b| a.0.cmp(&b.0).then(a.2.cmp(&b.2)).then(b.1.cmp(&a.1)));
        let mut chosen: Vec<(usize, usize, usize)> = Vec::new();
        let mut last_end = 0usize;
        for (s, e, idx) in cands {
            if s >= last_end {
                last_end = e;
                chosen.push((s, e, idx));
            }
        }

        let mut redacted = String::with_capacity(text.len());
        let mut counts = vec![0u32; self.patterns.len()];
        let mut pos = 0usize;
        for (s, e, idx) in chosen {
            redacted.push_str(&text[pos..s]);
            redacted.push_str("[REDACTED:");
            redacted.push_str(&self.patterns[idx].name);
            redacted.push(']');
            counts[idx] += 1;
            pos = e;
        }
        redacted.push_str(&text[pos..]);

        let entries = self
            .patterns
            .iter()
            .zip(counts)
            .filter(|(_, c)| *c > 0)
            .map(|(p, c)| RedactionEntry {
                pattern_name: p.name.clone(),
                count: c,
            })
            .collect();
        RedactionOutcome {
            redacted,
            log: RedactionLog {
                entries,
                original_len: text.len() as u64,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redactor() -> Redactor {
        Redactor::with_credential_defaults().unwrap()
    }

    #[test]
    fn redacts_bearer_token() {
        let r = redactor();
        let text = "call with Bearer abcDEF123-_~+/=xyz now";
        let out = r.redact(text);
        assert!(out.redacted.contains("[REDACTED:bearer-token]"));
        assert!(!out.redacted.contains("abcDEF123"));
        assert_eq!(out.log.total(), 1);
        assert_eq!(out.log.entries[0].pattern_name, "bearer-token");
        assert_eq!(out.log.original_len, text.len() as u64);
    }

    #[test]
    fn redacts_secret_assignment_case_insensitive() {
        let r = redactor();
        let out = r.redact("config: API_KEY=supersecretvalue123 done");
        assert!(out.redacted.contains("[REDACTED:secret-assignment]"));
        assert!(!out.redacted.contains("supersecretvalue123"));
        assert!(out.redacted.contains("done"));
    }

    #[test]
    fn clean_text_passes_through_with_empty_log() {
        let r = redactor();
        let out = r.redact("nothing sensitive here, just logs");
        assert_eq!(out.redacted, "nothing sensitive here, just logs");
        assert_eq!(out.log.total(), 0);
        assert!(out.log.entries.is_empty());
    }

    #[test]
    fn substring_pattern_works() {
        let r = Redactor::new(vec![
            RedactPattern::substring("internal-host", "db.internal.local").unwrap(),
        ])
        .unwrap();
        let out = r.redact("connect db.internal.local:5432 ok");
        assert_eq!(out.redacted, "connect [REDACTED:internal-host]:5432 ok");
    }

    #[test]
    fn overlapping_patterns_first_configured_wins() {
        let r = Redactor::new(vec![
            RedactPattern::substring("aaa", "aaa").unwrap(),
            RedactPattern::substring("aa", "aa").unwrap(),
        ])
        .unwrap();
        let out = r.redact("xaaay");
        assert_eq!(out.redacted, "x[REDACTED:aaa]y");
        assert_eq!(out.log.total(), 1);
    }

    #[test]
    fn multiple_occurrences_all_redacted_and_counted() {
        let r = Redactor::new(vec![RedactPattern::substring("tok", "TOKEN").unwrap()]).unwrap();
        let out = r.redact("TOKEN a TOKEN b TOKEN");
        assert_eq!(out.redacted, "[REDACTED:tok] a [REDACTED:tok] b [REDACTED:tok]");
        assert_eq!(out.log.entries[0].count, 3);
    }

    #[test]
    fn regex_quantifiers_and_classes() {
        let rx = RegexLite::compile("[0-9]{4}-[0-9]{2}").unwrap();
        assert_eq!(rx.find_leftmost("id 1234-56 ok"), Some((3, 10)));
        assert_eq!(rx.find_leftmost("nope 123-45"), None);
    }

    #[test]
    fn regex_anchors() {
        let rx = RegexLite::compile("^abc$").unwrap();
        assert!(rx.find_leftmost("abc").is_some());
        assert!(rx.find_leftmost("xabc").is_none());
        assert!(rx.find_leftmost("abcx").is_none());
    }

    #[test]
    fn regex_dot_star() {
        let rx = RegexLite::compile("a.*z").unwrap();
        assert_eq!(rx.find_leftmost("a12z"), Some((0, 4)));
    }

    #[test]
    fn regex_alternation() {
        let rx = RegexLite::compile("(cat|dog)s?").unwrap();
        assert_eq!(rx.find_leftmost("a cats"), Some((2, 6)));
        assert_eq!(rx.find_leftmost("a dog"), Some((2, 5)));
        assert_eq!(rx.find_leftmost("a bird"), None);
    }

    #[test]
    fn regex_group_quantified() {
        let rx = RegexLite::compile("(ab)+").unwrap();
        // Greedy: all three repetitions match.
        assert_eq!(rx.find_leftmost("xabababy"), Some((1, 7)));
        // Single repetition still matches (min is 1).
        assert_eq!(rx.find_leftmost("xaby"), Some((1, 3)));
        assert_eq!(rx.find_leftmost("xay"), None);
    }

    #[test]
    fn regex_unbalanced_group_rejected() {
        assert!(RegexLite::compile("(abc").is_err());
        assert!(RegexLite::compile("abc)").is_err());
        assert!(RegexLite::compile("()").is_err());
    }

    #[test]
    fn invalid_brace_quantifier_is_literal() {
        let rx = RegexLite::compile("a{b").unwrap();
        assert_eq!(rx.find_leftmost("xa{by"), Some((1, 4)));
        assert_eq!(rx.find_leftmost("aaab"), None);
    }

    #[test]
    fn invalid_patterns_rejected() {
        assert!(RegexLite::compile("").is_err());
        assert!(RegexLite::compile("[abc").is_err());
        assert!(RegexLite::compile("a\\").is_err());
        assert!(RegexLite::compile("[]").is_err());
        assert!(RedactPattern::substring("ok", "").is_err());
        assert!(RedactPattern::substring("bad name!", "x").is_err());
    }

    #[test]
    fn duplicate_pattern_names_rejected() {
        let err = Redactor::new(vec![
            RedactPattern::substring("dup", "a").unwrap(),
            RedactPattern::substring("dup", "b").unwrap(),
        ])
        .unwrap_err();
        assert!(matches!(err, FabricError::InvalidInput { .. }));
    }

    #[test]
    fn redaction_log_serde_stable() {
        let r = redactor();
        let out = r.redact("Bearer abcdefghijklmnop");
        let json = serde_json::to_string(&out.log).unwrap();
        assert!(json.contains("bearer-token"));
        assert!(!json.contains("abcdefghijklmnop")); // values never in the log
        let back: RedactionLog = serde_json::from_str(&json).unwrap();
        assert_eq!(back, out.log);
    }

    #[test]
    fn utf8_text_redacts_safely() {
        let r = Redactor::new(vec![RedactPattern::substring("s", "secret").unwrap()]).unwrap();
        let out = r.redact("héllo secret wörld");
        assert_eq!(out.redacted, "héllo [REDACTED:s] wörld");
    }
}
