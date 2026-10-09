//! T002: TaskClass enum with explicit unknown-class handling.
//!
//! The nine known classes serialize as their SCREAMING_SNAKE_CASE names.
//! Any other string deserializes to `Unknown(name)` instead of failing, so a
//! newer spec revision can introduce classes without breaking older readers.
//! Callers that require a known class use [`TaskClass::require_known`].

use crate::error::{FabricError, Result};

/// T002: the class of work a task performs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TaskClass {
    Plan,
    Implement,
    Review,
    Repair,
    TestGenerate,
    Adversarial,
    Integrate,
    Verify,
    Release,
    /// Explicit carrier for a class name this build does not recognize.
    Unknown(String),
}

impl TaskClass {
    /// All known class names in canonical order.
    pub const KNOWN: [&'static str; 9] = [
        "PLAN",
        "IMPLEMENT",
        "REVIEW",
        "REPAIR",
        "TEST_GENERATE",
        "ADVERSARIAL",
        "INTEGRATE",
        "VERIFY",
        "RELEASE",
    ];

    /// Parse a class name; unrecognized names become `Unknown`, never an error.
    pub fn parse(s: &str) -> Self {
        match s {
            "PLAN" => Self::Plan,
            "IMPLEMENT" => Self::Implement,
            "REVIEW" => Self::Review,
            "REPAIR" => Self::Repair,
            "TEST_GENERATE" => Self::TestGenerate,
            "ADVERSARIAL" => Self::Adversarial,
            "INTEGRATE" => Self::Integrate,
            "VERIFY" => Self::Verify,
            "RELEASE" => Self::Release,
            other => Self::Unknown(other.to_string()),
        }
    }

    /// Canonical name of this class.
    pub fn name(&self) -> &str {
        match self {
            Self::Plan => "PLAN",
            Self::Implement => "IMPLEMENT",
            Self::Review => "REVIEW",
            Self::Repair => "REPAIR",
            Self::TestGenerate => "TEST_GENERATE",
            Self::Adversarial => "ADVERSARIAL",
            Self::Integrate => "INTEGRATE",
            Self::Verify => "VERIFY",
            Self::Release => "RELEASE",
            Self::Unknown(s) => s.as_str(),
        }
    }

    /// True for the nine classes defined by the spec.
    pub fn is_known(&self) -> bool {
        !matches!(self, Self::Unknown(_))
    }

    /// Return the class or a typed error when it is unknown.
    pub fn require_known(&self) -> Result<&Self> {
        if self.is_known() {
            Ok(self)
        } else {
            Err(FabricError::UnknownClass(self.name().to_string()))
        }
    }
}

impl std::fmt::Display for TaskClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl serde::Serialize for TaskClass {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

impl<'de> serde::Deserialize<'de> for TaskClass {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(Self::parse(&raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_classes_round_trip() {
        for name in TaskClass::KNOWN {
            let c = TaskClass::parse(name);
            assert!(c.is_known());
            let json = serde_json::to_string(&c).unwrap();
            assert_eq!(json, format!("\"{name}\""));
            let back: TaskClass = serde_json::from_str(&json).unwrap();
            assert_eq!(c, back);
        }
    }

    #[test]
    fn unknown_class_is_explicit_not_an_error() {
        let c: TaskClass = serde_json::from_str("\"QUANTUM\"").unwrap();
        assert_eq!(c, TaskClass::Unknown("QUANTUM".to_string()));
        assert!(!c.is_known());
        assert_eq!(c.to_string(), "QUANTUM");
        // round-trips through the Unknown carrier
        let json = serde_json::to_string(&c).unwrap();
        let back: TaskClass = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn require_known_rejects_unknown() {
        let c = TaskClass::Unknown("NOPE".to_string());
        assert!(matches!(
            c.require_known(),
            Err(FabricError::UnknownClass(_))
        ));
        assert!(TaskClass::Verify.require_known().is_ok());
    }
}
