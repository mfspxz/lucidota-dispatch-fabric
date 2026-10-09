//! T066: validate worker output against the required schema.
//!
//! A worker's output is validated against the envelope's `output_schema`
//! (JSON Schema) *before* it can become a candidate artifact. Invalid output
//! is rejected with structured errors — never silently accepted, never
//! coerced into shape.
//!
//! This module implements a deterministic subset of JSON Schema (draft
//! 2020-12 semantics) sufficient for structural validation:
//! `type`, `enum`, `const`, `required`, `properties`, `patternProperties`,
//! `additionalProperties`, `items`, `prefixItems`, `min/maxItems`,
//! `uniqueItems`, `min/maxLength`, `pattern`, `minimum`, `maximum`,
//! `exclusiveMinimum`, `exclusiveMaximum`, `multipleOf`, `allOf`, `anyOf`,
//! `oneOf`, `not`, `if`/`then`/`else`, boolean schemas, and local `$ref`
//! (`#` and `#/...` JSON pointers only, recursion depth-bounded).
//! `format` is annotation-only and not asserted. Remote `$ref`s are rejected
//! at compile time.

use serde::{Deserialize, Serialize};
use std::fmt;

/// One structural violation, with a JSON-pointer-like instance path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationError {
    /// Instance location, e.g. `$`, `$.name`, `$[2].id`.
    pub path: String,
    /// The schema keyword that failed, e.g. `required`, `type`.
    pub keyword: String,
    pub message: String,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: [{}] {}", self.path, self.keyword, self.message)
    }
}

/// Schema compile failure (bad `$ref`, non-object schema, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaCompileError {
    pub message: String,
}

impl fmt::Display for SchemaCompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid output schema: {}", self.message)
    }
}

impl std::error::Error for SchemaCompileError {}

/// A pre-compiled output schema. Compile once per envelope, validate many outputs.
#[derive(Debug, Clone)]
pub struct OutputValidator {
    schema: serde_json::Value,
}

impl OutputValidator {
    /// Compile a schema. Rejects remote `$ref`s and non-object/non-boolean schemas.
    pub fn compile(schema: serde_json::Value) -> Result<Self, SchemaCompileError> {
        check_refs_local(&schema, "#")?;
        match &schema {
            serde_json::Value::Object(_) | serde_json::Value::Bool(_) => Ok(Self { schema }),
            _ => Err(SchemaCompileError {
                message: "schema must be an object or boolean".to_string(),
            }),
        }
    }

    /// Validate an instance. `Ok(())` = accepted as candidate artifact input;
    /// `Err(errors)` = rejected, with every violation listed.
    pub fn validate(&self, instance: &serde_json::Value) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();
        let mut ctx = Ctx {
            root: &self.schema,
            depth: 0,
        };
        ctx.validate_schema(&self.schema, instance, "$", &mut errors);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// One-shot validation without pre-compilation.
pub fn validate_output(
    schema: &serde_json::Value,
    output: &serde_json::Value,
) -> Result<(), Vec<ValidationError>> {
    match OutputValidator::compile(schema.clone()) {
        Ok(v) => v.validate(output),
        Err(e) => Err(vec![ValidationError {
            path: "$".to_string(),
            keyword: "$schema".to_string(),
            message: e.message,
        }]),
    }
}

fn check_refs_local(schema: &serde_json::Value, at: &str) -> Result<(), SchemaCompileError> {
    match schema {
        serde_json::Value::Object(map) => {
            if let Some(r) = map.get("$ref").and_then(|v| v.as_str()) {
                if !(r == "#" || r.starts_with("#/")) {
                    return Err(SchemaCompileError {
                        message: format!("remote $ref rejected at {at}: {r}"),
                    });
                }
            }
            for (k, v) in map {
                let child = format!("{at}/{k}");
                check_refs_local(v, &child)?;
            }
            Ok(())
        }
        serde_json::Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                check_refs_local(v, &format!("{at}[{i}]"))?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

const MAX_REF_DEPTH: usize = 32;

struct Ctx<'a> {
    root: &'a serde_json::Value,
    depth: usize,
}

impl<'a> Ctx<'a> {
    fn resolve_ref(&self, pointer: &str) -> Option<&'a serde_json::Value> {
        if pointer == "#" {
            return Some(self.root);
        }
        let mut cur = self.root;
        for seg in pointer.strip_prefix("#/")?.split('/') {
            let seg = seg.replace("~1", "/").replace("~0", "~");
            cur = cur.get(seg.as_str())?;
        }
        Some(cur)
    }

    fn validate_schema(
        &mut self,
        schema: &serde_json::Value,
        instance: &serde_json::Value,
        path: &str,
        errors: &mut Vec<ValidationError>,
    ) {
        let fail = |errors: &mut Vec<ValidationError>, keyword: &str, message: String| {
            errors.push(ValidationError {
                path: path.to_string(),
                keyword: keyword.to_string(),
                message,
            });
        };
        match schema {
            serde_json::Value::Bool(true) => {}
            serde_json::Value::Bool(false) => {
                fail(errors, "false-schema", "instance rejected by `false` schema".to_string());
            }
            serde_json::Value::Object(map) => {
                // $ref first (siblings apply too, per 2020-12).
                if let Some(r) = map.get("$ref").and_then(|v| v.as_str()) {
                    if self.depth >= MAX_REF_DEPTH {
                        fail(errors, "$ref", "reference depth limit exceeded".to_string());
                        return;
                    }
                    match self.resolve_ref(r) {
                        Some(target) => {
                            self.depth += 1;
                            self.validate_schema(target, instance, path, errors);
                            self.depth -= 1;
                        }
                        None => fail(errors, "$ref", format!("unresolvable reference: {r}")),
                    }
                }
                if let Some(t) = map.get("type") {
                    self.check_type(t, instance, path, errors);
                }
                if let Some(e) = map.get("enum") {
                    if let Some(arr) = e.as_array() {
                        if !arr.iter().any(|v| v == instance) {
                            fail(errors, "enum", "value not in enum".to_string());
                        }
                    }
                }
                if let Some(c) = map.get("const") {
                    if c != instance {
                        fail(errors, "const", "value does not equal const".to_string());
                    }
                }
                match instance {
                    serde_json::Value::Object(obj) => self.check_object(map, obj, path, errors),
                    serde_json::Value::Array(arr) => self.check_array(map, arr, path, errors),
                    serde_json::Value::String(s) => self.check_string(map, s, path, errors),
                    serde_json::Value::Number(n) => self.check_number(map, n, path, errors),
                    _ => {}
                }
                self.check_combinators(map, instance, path, errors);
                if let Some(cond) = map.get("if") {
                    let mut tmp = Vec::new();
                    let mut sub = Ctx { root: self.root, depth: self.depth + 1 };
                    sub.validate_schema(cond, instance, path, &mut tmp);
                    let branch = if tmp.is_empty() { "then" } else { "else" };
                    if let Some(b) = map.get(branch) {
                        self.validate_schema(b, instance, path, errors);
                    }
                }
                if let Some(n) = map.get("not") {
                    let mut tmp = Vec::new();
                    let mut sub = Ctx { root: self.root, depth: self.depth + 1 };
                    sub.validate_schema(n, instance, path, &mut tmp);
                    if tmp.is_empty() {
                        fail(errors, "not", "instance matches forbidden `not` schema".to_string());
                    }
                }
            }
            _ => fail(errors, "$schema", "schema must be object or boolean".to_string()),
        }
    }

    fn check_type(
        &self,
        t: &serde_json::Value,
        instance: &serde_json::Value,
        path: &str,
        errors: &mut Vec<ValidationError>,
    ) {
        let types: Vec<&str> = match t {
            serde_json::Value::String(s) => vec![s.as_str()],
            serde_json::Value::Array(a) => a.iter().filter_map(|v| v.as_str()).collect(),
            _ => {
                errors.push(ValidationError {
                    path: path.to_string(),
                    keyword: "type".to_string(),
                    message: "`type` must be a string or array of strings".to_string(),
                });
                return;
            }
        };
        let ok = types.iter().any(|ty| type_matches(ty, instance));
        if !ok {
            errors.push(ValidationError {
                path: path.to_string(),
                keyword: "type".to_string(),
                message: format!("expected {}, got {}", types.join("|"), json_kind(instance)),
            });
        }
    }

    fn check_object(
        &mut self,
        map: &serde_json::Map<String, serde_json::Value>,
        obj: &serde_json::Map<String, serde_json::Value>,
        path: &str,
        errors: &mut Vec<ValidationError>,
    ) {
        if let Some(req) = map.get("required").and_then(|v| v.as_array()) {
            for name in req.iter().filter_map(|v| v.as_str()) {
                if !obj.contains_key(name) {
                    errors.push(ValidationError {
                        path: path.to_string(),
                        keyword: "required".to_string(),
                        message: format!("missing required property `{name}`"),
                    });
                }
            }
        }
        let props = map.get("properties").and_then(|v| v.as_object());
        let patterns = map.get("patternProperties").and_then(|v| v.as_object());
        for (k, v) in obj {
            let child = format!("{path}.{k}");
            let mut matched = false;
            if let Some(ps) = props {
                if let Some(sub) = ps.get(k) {
                    matched = true;
                    self.validate_schema(sub, v, &child, errors);
                }
            }
            if !matched {
                if let Some(pats) = patterns {
                    for (pat, sub) in pats {
                        if regex_match(pat, k) {
                            matched = true;
                            self.validate_schema(sub, v, &child, errors);
                        }
                    }
                }
            }
            if !matched {
                match map.get("additionalProperties") {
                    Some(serde_json::Value::Bool(false)) => errors.push(ValidationError {
                        path: child,
                        keyword: "additionalProperties".to_string(),
                        message: format!("additional property `{k}` not allowed"),
                    }),
                    Some(sub @ serde_json::Value::Object(_)) | Some(sub @ serde_json::Value::Bool(_)) => {
                        self.validate_schema(sub, v, &child, errors)
                    }
                    _ => {}
                }
            }
        }
        for (k, limit) in [("minProperties", true), ("maxProperties", false)] {
            if let Some(n) = map.get(k).and_then(|v| v.as_u64()) {
                let bad = if limit { (obj.len() as u64) < n } else { (obj.len() as u64) > n };
                if bad {
                    errors.push(ValidationError {
                        path: path.to_string(),
                        keyword: k.to_string(),
                        message: format!("property count violates {k}={n}"),
                    });
                }
            }
        }
    }

    fn check_array(
        &mut self,
        map: &serde_json::Map<String, serde_json::Value>,
        arr: &[serde_json::Value],
        path: &str,
        errors: &mut Vec<ValidationError>,
    ) {
        if let Some(prefix) = map.get("prefixItems").and_then(|v| v.as_array()) {
            for (i, sub) in prefix.iter().enumerate() {
                if let Some(item) = arr.get(i) {
                    self.validate_schema(sub, item, &format!("{path}[{i}]"), errors);
                }
            }
            if map.get("items").is_none() {
                // no `items`: extra items beyond prefixItems are allowed.
            } else if let Some(items) = map.get("items") {
                for (i, item) in arr.iter().enumerate().skip(prefix.len()) {
                    self.validate_schema(items, item, &format!("{path}[{i}]"), errors);
                }
            }
        } else if let Some(items) = map.get("items") {
            for (i, item) in arr.iter().enumerate() {
                self.validate_schema(items, item, &format!("{path}[{i}]"), errors);
            }
        }
        for (k, is_min) in [("minItems", true), ("maxItems", false)] {
            if let Some(n) = map.get(k).and_then(|v| v.as_u64()) {
                let bad = if is_min { (arr.len() as u64) < n } else { (arr.len() as u64) > n };
                if bad {
                    errors.push(ValidationError {
                        path: path.to_string(),
                        keyword: k.to_string(),
                        message: format!("array length violates {k}={n}"),
                    });
                }
            }
        }
        if map.get("uniqueItems").and_then(|v| v.as_bool()) == Some(true) {
            for i in 0..arr.len() {
                for j in (i + 1)..arr.len() {
                    if arr[i] == arr[j] {
                        errors.push(ValidationError {
                            path: path.to_string(),
                            keyword: "uniqueItems".to_string(),
                            message: format!("duplicate items at [{i}] and [{j}]"),
                        });
                    }
                }
            }
        }
    }

    fn check_string(
        &self,
        map: &serde_json::Map<String, serde_json::Value>,
        s: &str,
        path: &str,
        errors: &mut Vec<ValidationError>,
    ) {
        let len = s.chars().count() as u64;
        for (k, is_min) in [("minLength", true), ("maxLength", false)] {
            if let Some(n) = map.get(k).and_then(|v| v.as_u64()) {
                let bad = if is_min { len < n } else { len > n };
                if bad {
                    errors.push(ValidationError {
                        path: path.to_string(),
                        keyword: k.to_string(),
                        message: format!("string length violates {k}={n}"),
                    });
                }
            }
        }
        if let Some(pat) = map.get("pattern").and_then(|v| v.as_str()) {
            if !regex_match(pat, s) {
                errors.push(ValidationError {
                    path: path.to_string(),
                    keyword: "pattern".to_string(),
                    message: "string does not match pattern".to_string(),
                });
            }
        }
    }

    fn check_number(
        &self,
        map: &serde_json::Map<String, serde_json::Value>,
        n: &serde_json::Number,
        path: &str,
        errors: &mut Vec<ValidationError>,
    ) {
        let f = n.as_f64().unwrap_or(f64::NAN);
        let check = |errors: &mut Vec<ValidationError>, keyword: &str, ok: bool, msg: String| {
            if !ok {
                errors.push(ValidationError {
                    path: path.to_string(),
                    keyword: keyword.to_string(),
                    message: msg,
                });
            }
        };
        if let Some(m) = map.get("minimum").and_then(|v| v.as_f64()) {
            check(errors, "minimum", f >= m, format!("{f} < minimum {m}"));
        }
        if let Some(m) = map.get("maximum").and_then(|v| v.as_f64()) {
            check(errors, "maximum", f <= m, format!("{f} > maximum {m}"));
        }
        if let Some(m) = map.get("exclusiveMinimum").and_then(|v| v.as_f64()) {
            check(errors, "exclusiveMinimum", f > m, format!("{f} <= exclusiveMinimum {m}"));
        }
        if let Some(m) = map.get("exclusiveMaximum").and_then(|v| v.as_f64()) {
            check(errors, "exclusiveMaximum", f < m, format!("{f} >= exclusiveMaximum {m}"));
        }
        if let Some(m) = map.get("multipleOf").and_then(|v| v.as_f64()) {
            if m != 0.0 {
                let q = f / m;
                check(errors, "multipleOf", (q - q.round()).abs() < 1e-9, format!("{f} not a multiple of {m}"));
            }
        }
    }

    /// Count how many branch schemas accept the instance. Immutable borrow
    /// ends on return, so callers may follow with `&mut self` validation.
    fn count_matching(
        &self,
        schemas: &serde_json::Value,
        instance: &serde_json::Value,
        path: &str,
    ) -> usize {
        schemas
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter(|s| {
                        let mut tmp = Vec::new();
                        let mut sub = Ctx {
                            root: self.root,
                            depth: self.depth + 1,
                        };
                        sub.validate_schema(s, instance, path, &mut tmp);
                        tmp.is_empty()
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    fn check_combinators(
        &mut self,
        map: &serde_json::Map<String, serde_json::Value>,
        instance: &serde_json::Value,
        path: &str,
        errors: &mut Vec<ValidationError>,
    ) {
        if let Some(all) = map.get("allOf") {
            if let Some(arr) = all.as_array() {
                for s in arr {
                    self.validate_schema(s, instance, path, errors);
                }
            }
        }
        if let Some(any) = map.get("anyOf") {
            if self.count_matching(any, instance, path) == 0 {
                errors.push(ValidationError {
                    path: path.to_string(),
                    keyword: "anyOf".to_string(),
                    message: "instance matches no anyOf branch".to_string(),
                });
            }
        }
        if let Some(one) = map.get("oneOf") {
            let n = self.count_matching(one, instance, path);
            if n != 1 {
                errors.push(ValidationError {
                    path: path.to_string(),
                    keyword: "oneOf".to_string(),
                    message: format!("instance matches {n} oneOf branches, expected exactly 1"),
                });
            }
        }
    }
}

fn type_matches(t: &str, v: &serde_json::Value) -> bool {
    match (t, v) {
        ("null", serde_json::Value::Null) => true,
        ("boolean", serde_json::Value::Bool(_)) => true,
        ("object", serde_json::Value::Object(_)) => true,
        ("array", serde_json::Value::Array(_)) => true,
        ("string", serde_json::Value::String(_)) => true,
        ("number", serde_json::Value::Number(_)) => true,
        ("integer", serde_json::Value::Number(n)) => {
            n.is_i64() || n.is_u64() || n.as_f64().is_some_and(|f| f.fract() == 0.0)
        }
        _ => false,
    }
}

fn json_kind(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Minimal regex support for `pattern` / `patternProperties` without an
/// external regex crate: handles the common structural cases used in output
/// schemas (literal match, `^`/`$` anchors, `.`, `*`, `+`, `?`, `\d`, `\w`,
/// `\s` and their negations, character classes `[...]`).
/// Anything unparseable falls back to substring semantics rather than
/// rejecting the instance — a validator must never fail closed on its own
/// pattern grammar.
fn regex_match(pattern: &str, text: &str) -> bool {
    match SimplePattern::parse(pattern) {
        Some(p) => p.matches_anywhere(text),
        None => text.contains(pattern),
    }
}

#[derive(Debug)]
enum Atom {
    Literal(char),
    Any,
    Digit(bool),
    Word(bool),
    Space(bool),
    Class(Vec<ClassItem>, bool),
}

#[derive(Debug)]
enum ClassItem {
    Char(char),
    Range(char, char),
}

#[derive(Debug)]
struct SimplePattern {
    atoms: Vec<(Atom, Quant)>,
    anchored_start: bool,
    anchored_end: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quant {
    One,
    ZeroOrOne,
    ZeroOrMore,
    OneOrMore,
}

impl SimplePattern {
    fn parse(pattern: &str) -> Option<Self> {
        let mut chars = pattern.chars().peekable();
        let mut anchored_start = false;
        let mut anchored_end = false;
        if chars.peek() == Some(&'^') {
            anchored_start = true;
            chars.next();
        }
        let mut atoms = Vec::new();
        while let Some(c) = chars.next() {
            if c == '$' && chars.peek().is_none() {
                anchored_end = true;
                continue;
            }
            let atom = match c {
                '\\' => match chars.next()? {
                    'd' => Atom::Digit(false),
                    'D' => Atom::Digit(true),
                    'w' => Atom::Word(false),
                    'W' => Atom::Word(true),
                    's' => Atom::Space(false),
                    'S' => Atom::Space(true),
                    e => Atom::Literal(e),
                },
                '.' => Atom::Any,
                '[' => {
                    let neg = chars.peek() == Some(&'^');
                    if neg {
                        chars.next();
                    }
                    let mut items = Vec::new();
                    while let Some(ch) = chars.next() {
                        if ch == ']' {
                            break;
                        }
                        let ch = if ch == '\\' { chars.next()? } else { ch };
                        if chars.peek() == Some(&'-') {
                            chars.next();
                            let end = chars.next()?;
                            let end = if end == '\\' { chars.next()? } else { end };
                            items.push(ClassItem::Range(ch, end));
                        } else {
                            items.push(ClassItem::Char(ch));
                        }
                    }
                    Atom::Class(items, neg)
                }
                '(' | ')' | '|' => return None, // groups/alternation: fall back
                _ => Atom::Literal(c),
            };
            let quant = match chars.peek() {
                Some('*') => {
                    chars.next();
                    Quant::ZeroOrMore
                }
                Some('+') => {
                    chars.next();
                    Quant::OneOrMore
                }
                Some('?') => {
                    chars.next();
                    Quant::ZeroOrOne
                }
                _ => Quant::One,
            };
            atoms.push((atom, quant));
        }
        Some(Self { atoms, anchored_start, anchored_end })
    }

    fn matches_anywhere(&self, text: &str) -> bool {
        let chars: Vec<char> = text.chars().collect();
        if self.anchored_start {
            return match self.match_at(&chars, 0) {
                Some(end) => !self.anchored_end || end == chars.len(),
                None => false,
            };
        }
        for start in 0..=chars.len() {
            if let Some(end) = self.match_at(&chars, start) {
                if !self.anchored_end || end == chars.len() {
                    return true;
                }
            }
        }
        false
    }

    fn match_at(&self, chars: &[char], pos: usize) -> Option<usize> {
        self.match_atoms(0, chars, pos)
    }

    fn match_atoms(&self, idx: usize, chars: &[char], pos: usize) -> Option<usize> {
        if idx == self.atoms.len() {
            return Some(pos);
        }
        let (atom, quant) = &self.atoms[idx];
        match quant {
            Quant::One => {
                let p = atom.consume(chars, pos)?;
                self.match_atoms(idx + 1, chars, p)
            }
            Quant::ZeroOrOne => {
                if let Some(p) = atom.consume(chars, pos) {
                    if let Some(r) = self.match_atoms(idx + 1, chars, p) {
                        return Some(r);
                    }
                }
                self.match_atoms(idx + 1, chars, pos)
            }
            Quant::ZeroOrMore | Quant::OneOrMore => {
                // Greedy with backtracking.
                let mut positions = vec![pos];
                let mut p = pos;
                while let Some(np) = atom.consume(chars, p) {
                    if np == p {
                        break;
                    }
                    p = np;
                    positions.push(p);
                }
                let min = if *quant == Quant::OneOrMore { 1 } else { 0 };
                if positions.len() - 1 < min {
                    return None;
                }
                for q in positions.iter().rev() {
                    if let Some(r) = self.match_atoms(idx + 1, chars, *q) {
                        return Some(r);
                    }
                }
                None
            }
        }
    }
}

impl Atom {
    fn consume(&self, chars: &[char], pos: usize) -> Option<usize> {
        let c = *chars.get(pos)?;
        let ok = match self {
            Atom::Literal(l) => c == *l,
            Atom::Any => true,
            Atom::Digit(neg) => c.is_ascii_digit() != *neg,
            Atom::Word(neg) => (c.is_ascii_alphanumeric() || c == '_') != *neg,
            Atom::Space(neg) => c.is_whitespace() != *neg,
            Atom::Class(items, neg) => {
                let hit = items.iter().any(|it| match it {
                    ClassItem::Char(x) => c == *x,
                    ClassItem::Range(a, b) => *a <= c && c <= *b,
                });
                hit != *neg
            }
        };
        ok.then_some(pos + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema(s: serde_json::Value) -> OutputValidator {
        OutputValidator::compile(s).expect("schema compiles")
    }

    #[test]
    fn accepts_valid_object() {
        let v = schema(json!({
            "type": "object",
            "required": ["name"],
            "properties": { "name": {"type": "string"}, "n": {"type": "integer"} },
            "additionalProperties": false
        }));
        assert!(v.validate(&json!({"name": "x", "n": 3})).is_ok());
    }

    #[test]
    fn rejects_missing_required_and_wrong_type() {
        let v = schema(json!({
            "type": "object",
            "required": ["name"],
            "properties": { "name": {"type": "string"} }
        }));
        let errs = v.validate(&json!({})).unwrap_err();
        assert!(errs.iter().any(|e| e.keyword == "required"));
        let errs = v.validate(&json!({"name": 42})).unwrap_err();
        assert!(errs.iter().any(|e| e.keyword == "type" && e.path == "$.name"));
    }

    #[test]
    fn rejects_additional_properties_when_forbidden() {
        let v = schema(json!({
            "type": "object",
            "properties": { "a": {"type": "string"} },
            "additionalProperties": false
        }));
        let errs = v.validate(&json!({"a": "x", "b": 1})).unwrap_err();
        assert!(errs.iter().any(|e| e.keyword == "additionalProperties"));
    }

    #[test]
    fn arrays_items_prefix_and_bounds() {
        let v = schema(json!({
            "type": "array",
            "prefixItems": [{"type": "string"}],
            "items": {"type": "integer"},
            "minItems": 2, "maxItems": 3, "uniqueItems": true
        }));
        assert!(v.validate(&json!(["a", 1, 2])).is_ok());
        assert!(v.validate(&json!(["a"])).is_err()); // minItems
        assert!(v.validate(&json!(["a", 1, 2, 3])).is_err()); // maxItems
        assert!(v.validate(&json!(["a", 1, 1])).is_err()); // uniqueItems
        assert!(v.validate(&json!([1, 2])).is_err()); // prefixItems type
    }

    #[test]
    fn string_and_number_constraints() {
        let v = schema(json!({
            "type": "object",
            "properties": {
                "s": {"type": "string", "minLength": 2, "maxLength": 4, "pattern": "^a+$"},
                "n": {"type": "number", "minimum": 0, "exclusiveMaximum": 10, "multipleOf": 0.5}
            }
        }));
        assert!(v.validate(&json!({"s": "aaa", "n": 2.5})).is_ok());
        let errs = v.validate(&json!({"s": "b", "n": 10})).unwrap_err();
        assert!(errs.len() >= 2);
    }

    #[test]
    fn combinators_and_not() {
        let v = schema(json!({"anyOf": [{"type": "string"}, {"type": "integer"}]}));
        assert!(v.validate(&json!("x")).is_ok());
        assert!(v.validate(&json!(1.5)).is_err());
        let v = schema(json!({"oneOf": [{"type": "number"}, {"type": "integer"}]}));
        assert!(v.validate(&json!(2)).is_err()); // matches both
        assert!(v.validate(&json!(2.5)).is_ok());
        let v = schema(json!({"not": {"type": "string"}}));
        assert!(v.validate(&json!(1)).is_ok());
        assert!(v.validate(&json!("x")).is_err());
    }

    #[test]
    fn local_ref_and_defs() {
        let v = schema(json!({
            "$defs": { "id": {"type": "string", "minLength": 1} },
            "type": "object",
            "properties": { "id": {"$ref": "#/$defs/id"} }
        }));
        assert!(v.validate(&json!({"id": "abc"})).is_ok());
        assert!(v.validate(&json!({"id": ""})).is_err());
    }

    #[test]
    fn remote_ref_rejected_at_compile() {
        let r = OutputValidator::compile(json!({"$ref": "https://example.com/s.json"}));
        assert!(r.is_err());
    }

    #[test]
    fn boolean_schemas() {
        assert!(schema(json!(true)).validate(&json!({"anything": 1})).is_ok());
        assert!(schema(json!(false)).validate(&json!({"anything": 1})).is_err());
    }

    #[test]
    fn enum_and_const() {
        let v = schema(json!({"enum": ["a", "b", 1]}));
        assert!(v.validate(&json!("a")).is_ok());
        assert!(v.validate(&json!("c")).is_err());
        let v = schema(json!({"const": 42}));
        assert!(v.validate(&json!(42)).is_ok());
        assert!(v.validate(&json!(43)).is_err());
    }

    #[test]
    fn if_then_else() {
        let v = schema(json!({
            "if": {"properties": {"kind": {"const": "a"}}},
            "then": {"required": ["x"]},
            "else": {"required": ["y"]}
        }));
        assert!(v.validate(&json!({"kind": "a", "x": 1})).is_ok());
        assert!(v.validate(&json!({"kind": "a"})).is_err());
        assert!(v.validate(&json!({"kind": "b", "y": 1})).is_ok());
    }

    #[test]
    fn invalid_output_is_rejected_never_coerced() {
        // one-shot path surfaces schema errors as validation errors
        let errs = validate_output(&json!({"type": "integer"}), &json!("nope")).unwrap_err();
        assert_eq!(errs[0].keyword, "type");
        // bad schema itself is reported, not panicked
        let errs = validate_output(&json!("nope"), &json!(1)).unwrap_err();
        assert_eq!(errs[0].keyword, "$schema");
    }

    #[test]
    fn pattern_properties() {
        let v = schema(json!({
            "type": "object",
            "patternProperties": { "^x-": {"type": "string"} }
        }));
        assert!(v.validate(&json!({"x-a": "ok"})).is_ok());
        assert!(v.validate(&json!({"x-a": 1})).is_err());
    }
}
