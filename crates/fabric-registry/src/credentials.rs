//! T029: credential resolution through secret references.
//!
//! Secrets never enter this module as values. A [`SecretRef`] names a
//! credential; the resolver maps the name to an opaque [`CredentialHandle`].
//! Actual secret material is injected at call time by the transport layer's
//! credential surrogate — outside this crate's memory model — so there is
//! nothing here to leak. `Debug`, `Display`, and any future serialization of
//! handles are redacted. [`redact_text`] scrubs candidate secret material
//! (`hsurr:` surrogates, `sk-` keys, `key = value` assignments) from logs,
//! prompts, and receipts.

use fabric_types::{FabricError, Result, SecretRef};
use std::collections::HashSet;
use std::fmt;

/// Opaque credential handle. Carries the reference name only — never a secret
/// value, because no secret value ever exists in this module.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct CredentialHandle {
    reference: String,
}

impl CredentialHandle {
    /// The reference name. Safe to log: it names the credential, not the secret.
    pub fn reference(&self) -> &str {
        &self.reference
    }
}

impl fmt::Debug for CredentialHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CredentialHandle(redacted)")
    }
}

impl fmt::Display for CredentialHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CredentialHandle(redacted)")
    }
}

/// Resolves secret references to opaque handles. Registration records the
/// reference NAME only, never a secret value.
#[derive(Debug, Default)]
pub struct CredentialResolver {
    registered: HashSet<String>,
}

impl CredentialResolver {
    /// Empty resolver: nothing is resolvable until registered.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a credential reference name the resolver may resolve.
    pub fn register_reference(&mut self, reference: &str) -> Result<()> {
        let r = SecretRef::new(reference)?;
        self.registered.insert(r.reference().to_string());
        Ok(())
    }

    /// Resolve a reference to an opaque handle. Unknown references are denied
    /// with [`FabricError::PolicyDenied`]; the handle still carries no value.
    pub fn resolve(&self, secret: &SecretRef) -> Result<CredentialHandle> {
        if self.registered.contains(secret.reference()) {
            Ok(CredentialHandle {
                reference: secret.reference().to_string(),
            })
        } else {
            Err(FabricError::PolicyDenied(format!(
                "credential reference '{}' is not registered",
                secret.reference()
            )))
        }
    }

    /// True when the reference is registered.
    pub fn is_registered(&self, secret: &SecretRef) -> bool {
        self.registered.contains(secret.reference())
    }
}

/// Scrub candidate secret material from free text (logs, prompts, receipts).
/// Redacts `hsurr:` surrogate tokens, `sk-` style keys, and `name = value`
/// assignments for key-like names (api_key, token, secret, password, bearer).
/// Never panics; text without candidate material passes through unchanged.
pub fn redact_text(text: &str) -> String {
    let out = redact_prefixed(text, "hsurr:");
    let out = redact_prefixed(&out, "sk-");
    redact_assignments(&out)
}

/// Replace `prefix` + following token characters with `prefix[REDACTED]`.
/// The prefix must start at a token boundary (not mid-word) so innocent
/// words like "disk-space" are never mangled.
fn redact_prefixed(text: &str, prefix: &str) -> String {
    let mut res = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(prefix) {
        let boundary_ok =
            i == 0 || !rest[..i].chars().next_back().is_some_and(|c| c.is_ascii_alphanumeric());
        if !boundary_ok {
            let end = i + prefix.len();
            res.push_str(&rest[..end]);
            rest = &rest[end..];
            continue;
        }
        res.push_str(&rest[..i + prefix.len()]);
        let after = &rest[i + prefix.len()..];
        let tok_len: usize = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            .map(|c| c.len_utf8())
            .sum();
        res.push_str("[REDACTED]");
        rest = &after[tok_len..];
    }
    res.push_str(rest);
    res
}

const SENSITIVE_KEYS: [&str; 6] = [
    "api_key", "apikey", "token", "secret", "password", "bearer",
];

/// Replace `key [: =] value` with `key [REDACTED]` for key-like names.
/// Handles optional whitespace and single/double-quoted values.
fn redact_assignments(text: &str) -> String {
    let lower = text.to_lowercase();
    let lbytes = lower.as_bytes();
    let bytes = text.as_bytes();
    let mut res = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let mut matched = false;
        for key in SENSITIVE_KEYS {
            let kb = key.as_bytes();
            if lbytes.len() >= i + kb.len()
                && &lbytes[i..i + kb.len()] == kb
                && (i == 0 || !lbytes[i - 1].is_ascii_alphanumeric())
            {
                let mut j = i + kb.len();
                while j < text.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < text.len() && (bytes[j] == b':' || bytes[j] == b'=') {
                    j += 1;
                    while j < text.len() && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    let vstart = j;
                    if j < text.len() && (bytes[j] == b'"' || bytes[j] == b'\'') {
                        let q = bytes[j];
                        j += 1;
                        while j < text.len() && bytes[j] != q {
                            j += 1;
                        }
                        if j < text.len() {
                            j += 1; // closing quote
                        }
                    } else {
                        while j < text.len() && !bytes[j].is_ascii_whitespace() {
                            j += 1;
                        }
                    }
                    res.push_str(&text[i..vstart]);
                    res.push_str("[REDACTED]");
                    i = j;
                    matched = true;
                    break;
                }
            }
        }
        if !matched {
            // copy one char (UTF-8 safe); i is always a char boundary here
            let ch = text[i..].chars().next().unwrap_or('\u{FFFD}');
            res.push(ch);
            i += ch.len_utf8().max(1);
        }
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_debug_and_display_are_redacted() {
        let mut r = CredentialResolver::new();
        r.register_reference("custom.deepseek").unwrap();
        let h = r.resolve(&SecretRef::new("custom.deepseek").unwrap()).unwrap();
        assert_eq!(format!("{h:?}"), "CredentialHandle(redacted)");
        assert_eq!(h.to_string(), "CredentialHandle(redacted)");
        assert!(!format!("{h:?}").contains("deepseek"));
        // the reference NAME is safe to expose; the value never exists
        assert_eq!(h.reference(), "custom.deepseek");
    }

    #[test]
    fn unregistered_reference_is_denied() {
        let r = CredentialResolver::new();
        let err = r.resolve(&SecretRef::new("custom.deepseek").unwrap());
        assert!(matches!(err, Err(FabricError::PolicyDenied(_))));
        assert!(!r.is_registered(&SecretRef::new("custom.deepseek").unwrap()));
    }

    #[test]
    fn register_rejects_invalid_names() {
        let mut r = CredentialResolver::new();
        assert!(r.register_reference("").is_err());
        assert!(r.register_reference("has space").is_err());
    }

    #[test]
    fn redact_surrogates_and_keys() {
        assert_eq!(
            redact_text("auth=hsurr:abc123XYZ rest"),
            "auth=hsurr:[REDACTED] rest"
        );
        assert_eq!(
            redact_text("key sk-abcdef1234567890"),
            "key sk-[REDACTED]"
        );
    }

    #[test]
    fn redact_assignments() {
        assert_eq!(redact_text("api_key: hunter2"), "api_key: [REDACTED]");
        assert_eq!(redact_text("token=abc123"), "token=[REDACTED]");
        assert_eq!(
            redact_text("password = \"s3cr3t!\""),
            "password = [REDACTED]"
        );
        assert_eq!(
            redact_text("bearer_token=xyz"),
            "bearer_token=[REDACTED]"
        );
    }

    #[test]
    fn redact_leaves_innocent_text_alone() {
        let t = "the token effort was monumental; no secrets here. café ☕";
        assert_eq!(redact_text(t), t);
    }

    #[test]
    fn redact_does_not_match_substrings() {
        // "tokens" as a word still matches the key "token" at a word boundary;
        // "mytoken=x" must NOT match (no boundary before 'token').
        assert_eq!(redact_text("mytoken=x"), "mytoken=x");
    }

    #[test]
    fn redact_does_not_mangle_innocent_words() {
        // "sk-" inside "disk-space" is not a key prefix: word boundary required.
        assert_eq!(redact_text("disk-space is free"), "disk-space is free");
        // ...but a real standalone sk- key is still redacted.
        assert_eq!(redact_text("sk-live-abc123"), "sk-[REDACTED]");
    }
}
