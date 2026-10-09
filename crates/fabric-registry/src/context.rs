//! T028: context usage estimation and enforcement.
//!
//! [`estimate_usage`] converts character counts to token estimates with a
//! documented heuristic ([`CHARS_PER_TOKEN`]); estimates are labeled as such
//! and never presented as measurements. [`check_fits`] rejects work whose
//! estimate exceeds the endpoint's context window; [`truncate_prompt_to_fit`]
//! transforms an oversized prompt down to the largest size that fits.

use crate::endpoints::Endpoint;
use fabric_types::{FabricError, Result};

/// Characters per token used by [`estimate_usage`]. Documented heuristic for
/// typical text, not a measurement.
pub const CHARS_PER_TOKEN: u64 = 4;

/// Token usage estimate for one inference call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageEstimate {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Estimate token usage from character counts (heuristic, see
/// [`CHARS_PER_TOKEN`]). Rounds up; zero chars estimate zero tokens.
pub fn estimate_usage(prompt_chars: u64, expected_output_chars: u64) -> UsageEstimate {
    UsageEstimate {
        input_tokens: prompt_chars.div_ceil(CHARS_PER_TOKEN),
        output_tokens: expected_output_chars.div_ceil(CHARS_PER_TOKEN),
    }
}

/// Reject an estimate that exceeds the endpoint's context window, in either
/// the input or the output dimension.
pub fn check_fits(endpoint: &Endpoint, est: &UsageEstimate) -> Result<()> {
    if est.input_tokens > endpoint.context_window.input_tokens {
        return Err(FabricError::InvalidInput {
            field: "context_budget",
            reason: format!(
                "estimated input tokens {} exceed endpoint '{}' window of {}",
                est.input_tokens, endpoint.id, endpoint.context_window.input_tokens
            ),
        });
    }
    if est.output_tokens > endpoint.context_window.output_tokens {
        return Err(FabricError::InvalidInput {
            field: "context_budget",
            reason: format!(
                "estimated output tokens {} exceed endpoint '{}' window of {}",
                est.output_tokens, endpoint.id, endpoint.context_window.output_tokens
            ),
        });
    }
    Ok(())
}

/// Transform: return the largest prompt size in chars that fits the endpoint's
/// input window. The output reservation is checked first: when even the
/// expected output alone exceeds the output window, no prompt can fit and this
/// errors instead of silently producing an unusable truncation.
pub fn truncate_prompt_to_fit(
    endpoint: &Endpoint,
    expected_output_tokens: u64,
    prompt_chars: u64,
) -> Result<u64> {
    if expected_output_tokens > endpoint.context_window.output_tokens {
        return Err(FabricError::InvalidInput {
            field: "context_budget",
            reason: format!(
                "expected output tokens {} alone exceed endpoint '{}' output window of {}",
                expected_output_tokens, endpoint.id, endpoint.context_window.output_tokens
            ),
        });
    }
    let fit_chars = endpoint
        .context_window
        .input_tokens
        .saturating_mul(CHARS_PER_TOKEN);
    Ok(prompt_chars.min(fit_chars))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoints::{ContextWindow, TransportKind};
    use fabric_types::{EndpointId, ModelId, ProviderId, QualityTier};

    fn endpoint(in_tok: u64, out_tok: u64) -> Endpoint {
        Endpoint {
            id: EndpointId::new("ep-a").unwrap(),
            provider: ProviderId::new("p").unwrap(),
            model: ModelId::new("m").unwrap(),
            transport: TransportKind::Http,
            capabilities: vec![],
            context_window: ContextWindow {
                input_tokens: in_tok,
                output_tokens: out_tok,
            },
            structured_output: false,
            max_quality_tier: QualityTier::High,
            enabled: true,
        }
    }

    #[test]
    fn estimate_rounds_up() {
        assert_eq!(
            estimate_usage(0, 0),
            UsageEstimate {
                input_tokens: 0,
                output_tokens: 0
            }
        );
        assert_eq!(estimate_usage(1, 1).input_tokens, 1);
        assert_eq!(estimate_usage(8, 5).input_tokens, 2);
        assert_eq!(estimate_usage(8, 5).output_tokens, 2);
    }

    #[test]
    fn check_fits_accepts_within_window() {
        let e = endpoint(1000, 500);
        assert!(check_fits(&e, &UsageEstimate {
            input_tokens: 1000,
            output_tokens: 500
        })
        .is_ok());
    }

    #[test]
    fn check_fits_rejects_each_dimension() {
        let e = endpoint(1000, 500);
        assert!(check_fits(
            &e,
            &UsageEstimate {
                input_tokens: 1001,
                output_tokens: 10
            }
        )
        .is_err());
        assert!(check_fits(
            &e,
            &UsageEstimate {
                input_tokens: 10,
                output_tokens: 501
            }
        )
        .is_err());
    }

    #[test]
    fn truncate_keeps_fitting_prompts() {
        let e = endpoint(1000, 500);
        assert_eq!(truncate_prompt_to_fit(&e, 100, 100).unwrap(), 100);
    }

    #[test]
    fn truncate_clamps_oversized_prompts() {
        let e = endpoint(1000, 500);
        // 1000 tokens * 4 chars/token = 4000 char budget
        assert_eq!(truncate_prompt_to_fit(&e, 100, 999_999).unwrap(), 4000);
    }

    #[test]
    fn truncate_rejects_impossible_output_reservation() {
        let e = endpoint(1000, 500);
        assert!(truncate_prompt_to_fit(&e, 501, 10).is_err());
    }
}
