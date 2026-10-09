//! T001: validated, immutable TaskId and SpecId types.
//!
//! Stable serialization (plain string), total ordering, hashing, and display
//! all derive from the inner string. Validation runs at construction and at
//! deserialization, so an invalid ID can never enter the system through any path.

use crate::error::{FabricError, Result};

/// Validate the shared identifier grammar: 1..=128 chars, starts with an
/// ASCII letter, remaining chars in `[A-Za-z0-9-_.:]`.
pub fn validate_id(s: &str, field: &'static str) -> Result<()> {
    if s.is_empty() || s.len() > 128 {
        return Err(FabricError::InvalidInput {
            field,
            reason: "length must be 1..=128 characters".to_string(),
        });
    }
    let mut chars = s.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return Err(FabricError::InvalidInput {
            field,
            reason: "must start with an ASCII letter".to_string(),
        });
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
    {
        return Err(FabricError::InvalidInput {
            field,
            reason: "charset is [A-Za-z0-9-_.:]".to_string(),
        });
    }
    Ok(())
}

macro_rules! define_id {
    ($name:ident, $field:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Construct a validated, immutable identifier.
            pub fn new(s: impl Into<String>) -> Result<Self> {
                let s = s.into();
                validate_id(&s, $field)?;
                Ok(Self(s))
            }

            /// Borrow the identifier text.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl std::str::FromStr for $name {
            type Err = FabricError;
            fn from_str(s: &str) -> Result<Self> {
                Self::new(s)
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(
                deserializer: D,
            ) -> std::result::Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                Self::new(raw).map_err(serde::de::Error::custom)
            }
        }
    };
}

define_id!(
    TaskId,
    "task_id",
    "T001: validated immutable task identifier with stable string serialization."
);
define_id!(
    SpecId,
    "spec_id",
    "T001: validated immutable specification identifier with stable string serialization."
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_well_formed_ids() {
        for good in ["T001", "task-abc_123", "spec:v2026-10-09", "a.b:c-d_e"] {
            assert!(TaskId::new(good).is_ok(), "{good}");
            assert!(SpecId::new(good).is_ok(), "{good}");
        }
    }

    #[test]
    fn rejects_malformed_ids() {
        for bad in ["", "1abc", "-abc", "has space", "semi;colon", "ünicode"] {
            assert!(TaskId::new(bad).is_err(), "{bad:?}");
        }
        assert!(TaskId::new("a".repeat(129)).is_err());
    }

    #[test]
    fn ordering_hashing_display_are_stable() {
        let a = TaskId::new("T001").unwrap();
        let b = TaskId::new("T002").unwrap();
        assert!(a < b);
        assert_eq!(a.to_string(), "T001");
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(json, "\"T001\"");
        let back: TaskId = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn deserialization_rejects_invalid_ids() {
        let bad: std::result::Result<TaskId, _> = serde_json::from_str("\"1bad\"");
        assert!(bad.is_err());
    }

    #[test]
    fn ids_are_immutable_and_distinct_types() {
        let t = TaskId::new("T001").unwrap();
        let s = SpecId::new("T001").unwrap();
        assert_eq!(t.as_str(), s.as_str());
        // distinct types: this would not compile if unified
        fn takes_task(_: &TaskId) {}
        takes_task(&t);
    }
}
