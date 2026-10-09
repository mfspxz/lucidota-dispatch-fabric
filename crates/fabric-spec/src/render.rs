//! T019: render deterministic one-shot worker prompts.
//!
//! Compartmentation rule: a rendered prompt contains ONLY the task contract,
//! the relevant requirements, the shared-contract identifiers, and the output
//! schema. It never carries project-wide context, other tasks' objectives,
//! endpoint details, or credentials. The worker gets its organelle and nothing
//! else.
//!
//! Determinism rule: the same inputs always render byte-identical text.
//! Collections are sorted before rendering; the sha256 digest in
//! [`RenderedPrompt::digest`] pins the exact bytes rendered.

use crate::parser::Requirement;
use fabric_types::{Result, TaskClass, TaskContract};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Identifiers for the shared contracts, never their contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedContracts {
    pub fabric_version: String,
    pub policy_digest: String,
}

/// A rendered prompt plus its content digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedPrompt {
    pub text: String,
    /// `sha256:<hex>` of the exact rendered bytes.
    pub digest: String,
}

fn sha256_hex(text: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(text.as_bytes()))
}

fn risk_name(contract: &TaskContract) -> &'static str {
    match contract.risk_tier {
        fabric_types::RiskTier::Low => "LOW",
        fabric_types::RiskTier::Medium => "MEDIUM",
        fabric_types::RiskTier::High => "HIGH",
        fabric_types::RiskTier::Critical => "CRITICAL",
    }
}

fn class_name(class: &TaskClass) -> &str {
    class.name()
}

/// Render a one-shot worker prompt.
///
/// * `contract` — the task contract (the worker's whole world).
/// * `requirements` — ONLY the requirements relevant to this task. The caller
///   selects them; anything not passed is not included, by construction.
/// * `shared` — shared-contract identifiers (versions/digests, not contents).
/// * `output_schema` — the JSON schema the worker's output must satisfy.
///
/// The output is deterministic: same inputs → byte-identical text.
pub fn render_prompt(
    contract: &TaskContract,
    requirements: &[Requirement],
    shared: &SharedContracts,
    output_schema: &serde_json::Value,
) -> Result<RenderedPrompt> {
    let mut inputs = contract.inputs.clone();
    inputs.sort();
    let mut outputs = contract.outputs.clone();
    outputs.sort();
    let mut deps: Vec<String> = contract
        .dependencies
        .iter()
        .map(|d| d.as_str().to_string())
        .collect();
    deps.sort();
    let mut scope = contract.scope.allowed_paths.clone();
    scope.sort();
    let mut acceptance = contract.acceptance.clone();
    acceptance.sort_by(|a, b| a.id.cmp(&b.id));
    let mut verifiers = contract.verifiers.clone();
    verifiers.sort();

    // Requirements sorted by source anchor for stability.
    let mut reqs = requirements.to_vec();
    reqs.sort_by_key(|r| (r.anchor.line_start, r.anchor.line_end));

    let mut text = String::new();
    text.push_str(&format!("# WORKER TASK {}\n\n", contract.id.as_str()));
    text.push_str(&format!("class: {}\n", class_name(&contract.class)));
    text.push_str(&format!("spec_revision: {}\n", contract.spec_revision));
    text.push_str(&format!("fabric_version: {}\n", shared.fabric_version));
    text.push_str(&format!("policy_digest: {}\n\n", shared.policy_digest));

    let mut section = |title: &str, body: &str| {
        text.push_str("## ");
        text.push_str(title);
        text.push('\n');
        text.push_str(body);
        if !body.ends_with('\n') {
            text.push('\n');
        }
        text.push('\n');
    };

    section("OBJECTIVE", &contract.objective);
    section(
        "INPUTS",
        &inputs.iter().map(|i| format!("- {i}")).collect::<Vec<_>>().join("\n"),
    );
    section(
        "EXPECTED OUTPUTS",
        &outputs.iter().map(|o| format!("- {o}")).collect::<Vec<_>>().join("\n"),
    );
    section(
        "DEPENDENCIES (already accepted; do not re-derive them)",
        &deps.iter().map(|d| format!("- {d}")).collect::<Vec<_>>().join("\n"),
    );
    section(
        "SCOPE (allowed paths only; everything else is forbidden)",
        &scope.iter().map(|s| format!("- {s}")).collect::<Vec<_>>().join("\n"),
    );
    section(
        "ACCEPTANCE PREDICATES",
        &acceptance
            .iter()
            .map(|p| {
                format!(
                    "- [{}] {} (evidence: {})",
                    p.id,
                    p.description,
                    if p.evidence_required { "required" } else { "optional" }
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
    );
    section(
        "VERIFIERS",
        &verifiers.iter().map(|v| format!("- {v}")).collect::<Vec<_>>().join("\n"),
    );
    section(
        "RETRY POLICY",
        &format!(
            "max_attempts={} backoff={:?} base_delay_ms={} retry_budget={}",
            contract.retry.max_attempts,
            contract.retry.backoff,
            contract.retry.base_delay_ms,
            contract.retry.retry_budget
        ),
    );
    section("RISK TIER", risk_name(contract));

    let req_body = reqs
        .iter()
        .map(|r| {
            format!(
                "### lines {}-{}\n{}",
                r.anchor.line_start, r.anchor.line_end, r.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    section("RELEVANT REQUIREMENTS", &req_body);

    let schema = serde_json::to_string_pretty(output_schema)
        .unwrap_or_else(|_| output_schema.to_string());
    section("OUTPUT SCHEMA (your output must satisfy this)", &schema);

    section(
        "RULES",
        "- Produce only the outputs listed above, inside the allowed scope.\n\
         - Every acceptance predicate marked evidence-required must cite evidence; claims without evidence are rejected.\n\
         - Do not access paths outside SCOPE. Do not exfiltrate credentials.\n\
         - This prompt is compartmented: you have no knowledge of other tasks or the wider project. Do not ask for it.",
    );

    let digest = sha256_hex(&text);
    Ok(RenderedPrompt { text, digest })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::SourceAnchor;
    use fabric_types::{AcceptancePredicate, Scope, TaskClass, TaskContract, TaskId};

    fn contract() -> TaskContract {
        TaskContract::builder(TaskId::new("T019").unwrap(), TaskClass::Implement, "Render prompts.")
            .spec_revision("master-spec v2026-10-09")
            .inputs(vec!["b".to_string(), "a".to_string()])
            .outputs(vec!["T019.out".to_string()])
            .scope(Scope::new(vec!["crates/fabric-spec".to_string()]).unwrap())
            .acceptance(vec![AcceptancePredicate::new("p1", "deterministic", true).unwrap()])
            .verifiers(vec!["cargo-test".to_string()])
            .build()
            .unwrap()
    }

    fn shared() -> SharedContracts {
        SharedContracts {
            fabric_version: "0.1.0".to_string(),
            policy_digest: "sha256:abc".to_string(),
        }
    }

    fn schema() -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    fn requirement() -> Requirement {
        Requirement {
            anchor: SourceAnchor { line_start: 10, line_end: 12 },
            text: "Prompts must be deterministic.".to_string(),
        }
    }

    #[test]
    fn same_inputs_byte_identical() {
        let c = contract();
        let p1 = render_prompt(&c, &[requirement()], &shared(), &schema()).unwrap();
        let p2 = render_prompt(&c, &[requirement()], &shared(), &schema()).unwrap();
        assert_eq!(p1.text, p2.text);
        assert_eq!(p1.digest, p2.digest);
    }

    #[test]
    fn digest_matches_text() {
        let p = render_prompt(&contract(), &[], &shared(), &schema()).unwrap();
        assert_eq!(p.digest, sha256_hex(&p.text));
        assert!(p.digest.starts_with("sha256:"));
        assert_eq!(p.digest.len(), 7 + 64);
    }

    #[test]
    fn input_order_does_not_change_render() {
        let mut c1 = contract();
        let mut c2 = contract();
        c1.inputs = vec!["a".to_string(), "b".to_string()];
        c2.inputs = vec!["b".to_string(), "a".to_string()];
        let p1 = render_prompt(&c1, &[], &shared(), &schema()).unwrap();
        let p2 = render_prompt(&c2, &[], &shared(), &schema()).unwrap();
        assert_eq!(p1.text, p2.text);
    }

    #[test]
    fn compartmented_no_leakage() {
        // Strings that exist nowhere in the inputs must not appear in the prompt.
        let p = render_prompt(&contract(), &[requirement()], &shared(), &schema()).unwrap();
        for secret in [
            "TOP-SECRET-PROJECT",
            "another task's objective",
            "custom.deepseek",
            "nvidia-nim",
        ] {
            assert!(!p.text.contains(secret), "leaked: {secret}");
        }
        // And only the relevant requirement is present.
        assert!(p.text.contains("Prompts must be deterministic."));
    }

    #[test]
    fn contains_contract_essentials() {
        let p = render_prompt(&contract(), &[requirement()], &shared(), &schema()).unwrap();
        assert!(p.text.contains("# WORKER TASK T019"));
        assert!(p.text.contains("Render prompts."));
        assert!(p.text.contains("crates/fabric-spec"));
        assert!(p.text.contains("sha256:abc"));
    }
}
