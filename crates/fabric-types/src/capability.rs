//! T003: CapabilityRequirement.
//!
//! What a task needs from an inference endpoint: capability names, quality
//! tier, context budget, structured-output needs, and optional tool needs.

use crate::error::{FabricError, Result};
use crate::ids::validate_id;
use serde::{Deserialize, Serialize};

/// Quality tier a task demands from an endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum QualityTier {
    Low,
    Standard,
    High,
}

/// Token budget for a task's prompt and expected output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContextBudget {
    /// Maximum input tokens the endpoint must accept, if bounded.
    pub max_input_tokens: Option<u64>,
    /// Maximum output tokens the task may produce, if bounded.
    pub max_output_tokens: Option<u64>,
}

impl ContextBudget {
    /// Unbounded budget: any endpoint qualifies on context.
    pub fn unbounded() -> Self {
        Self {
            max_input_tokens: None,
            max_output_tokens: None,
        }
    }

    /// Validate: present limits must be positive.
    pub fn validate(&self) -> Result<()> {
        for (field, v) in [
            ("max_input_tokens", self.max_input_tokens),
            ("max_output_tokens", self.max_output_tokens),
        ] {
            if v == Some(0) {
                return Err(FabricError::InvalidInput {
                    field,
                    reason: "token budget must be positive when set".to_string(),
                });
            }
        }
        Ok(())
    }
}

/// T003: what a task requires from an endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityRequirement {
    /// Required capability names, e.g. `coding`, `review`. Non-empty.
    pub capabilities: Vec<String>,
    pub quality_tier: QualityTier,
    pub context_budget: ContextBudget,
    /// Whether the endpoint must support structured output.
    pub structured_output: bool,
    /// Optional schema reference for structured output.
    pub output_schema: Option<String>,
    /// Optional tool requirements, e.g. `browser`, `exec`.
    pub tools: Vec<String>,
}

impl CapabilityRequirement {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        capabilities: Vec<String>,
        quality_tier: QualityTier,
        context_budget: ContextBudget,
        structured_output: bool,
        output_schema: Option<String>,
        tools: Vec<String>,
    ) -> Result<Self> {
        if capabilities.is_empty() {
            return Err(FabricError::InvalidInput {
                field: "capabilities",
                reason: "at least one capability is required".to_string(),
            });
        }
        for c in &capabilities {
            validate_id(c, "capabilities")?;
        }
        for t in &tools {
            validate_id(t, "tools")?;
        }
        context_budget.validate()?;
        Ok(Self {
            capabilities,
            quality_tier,
            context_budget,
            structured_output,
            output_schema,
            tools,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> CapabilityRequirement {
        CapabilityRequirement::new(
            vec!["coding".to_string()],
            QualityTier::Standard,
            ContextBudget::unbounded(),
            false,
            None,
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn rejects_empty_capabilities() {
        let r = CapabilityRequirement::new(
            vec![],
            QualityTier::Standard,
            ContextBudget::unbounded(),
            false,
            None,
            vec![],
        );
        assert!(r.is_err());
    }

    #[test]
    fn rejects_zero_token_budget() {
        let r = CapabilityRequirement::new(
            vec!["coding".to_string()],
            QualityTier::Standard,
            ContextBudget {
                max_input_tokens: Some(0),
                max_output_tokens: None,
            },
            false,
            None,
            vec![],
        );
        assert!(r.is_err());
    }

    #[test]
    fn round_trips_through_json() {
        let r = req();
        let json = serde_json::to_string(&r).unwrap();
        let back: CapabilityRequirement = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }
}
