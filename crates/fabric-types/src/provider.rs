//! T009: ProviderId, ModelId, EndpointId, and credential references.
//!
//! Hard rule: secrets never appear in `Debug` output or serialization.
//! [`SecretRef`] holds only a *reference name* (e.g. `custom.deepseek`); the
//! type cannot hold a secret value at all, so leakage is impossible by
//! construction rather than by convention.

use crate::error::Result;
use crate::ids::validate_id;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};

macro_rules! define_provider_id {
    ($name:ident, $field:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Result<Self> {
                let s = s.into();
                validate_id(&s, $field)?;
                Ok(Self(s))
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
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

define_provider_id!(
    ProviderId,
    "provider_id",
    "T009: validated provider identifier, e.g. `deepseek`, `nvidia-nim`."
);
define_provider_id!(
    ModelId,
    "model_id",
    "T009: validated model identifier, e.g. `deepseek-v4-pro`."
);
define_provider_id!(
    EndpointId,
    "endpoint_id",
    "T009: validated endpoint identifier for a registered inference endpoint."
);

/// T009: reference to a credential by name. Never holds a secret value.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SecretRef {
    reference: String,
}

impl SecretRef {
    /// Bind a credential reference name (e.g. `custom.deepseek`).
    /// The referenced secret itself is resolved at call time by the
    /// credential surrogate layer and never enters this type.
    pub fn new(reference: impl Into<String>) -> Result<Self> {
        let reference = reference.into();
        validate_id(&reference, "secret_ref")?;
        Ok(Self { reference })
    }

    /// The reference name (safe to log: it names the credential, not the secret).
    pub fn reference(&self) -> &str {
        &self.reference
    }
}

/// Debug is redacted: even the reference name stays out of debug dumps.
impl std::fmt::Debug for SecretRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretRef(redacted)")
    }
}

impl std::fmt::Display for SecretRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretRef(redacted)")
    }
}

/// Serialization emits only the reference name; there is no value to leak.
impl Serialize for SecretRef {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("SecretRef", 1)?;
        s.serialize_field("ref", &self.reference)?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for SecretRef {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Helper {
            #[serde(rename = "ref")]
            reference: String,
        }
        let h = Helper::deserialize(deserializer)?;
        Self::new(h.reference).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_display_are_redacted() {
        let r = SecretRef::new("custom.deepseek").unwrap();
        assert_eq!(format!("{r:?}"), "SecretRef(redacted)");
        assert_eq!(r.to_string(), "SecretRef(redacted)");
        assert!(!format!("{r:?}").contains("deepseek"));
    }

    #[test]
    fn serialization_carries_no_value() {
        let r = SecretRef::new("custom.deepseek").unwrap();
        let j = serde_json::to_string(&r).unwrap();
        assert_eq!(j, "{\"ref\":\"custom.deepseek\"}");
        let back: SecretRef = serde_json::from_str(&j).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn provider_ids_validate() {
        assert!(ProviderId::new("nvidia-nim").is_ok());
        assert!(ProviderId::new("").is_err());
        assert!(ModelId::new("deepseek-v4-pro").is_ok());
        assert!(EndpointId::new("ep-1").is_ok());
    }
}
