//! REG-03 — Composition planning (cell ORG2).
//!
//! Backward/forward planner over registered organelles: given a target
//! output `TypeRef` and a set of available input `TypeRef`s, search for a
//! chain of organelles whose wiring is type-compatible. Wiring follows the
//! same wire-time rule as `organelle_core::Composed::wire`: exact `TypeRef`
//! equality, checked at plan time (never at run time).
//!
//! The planner takes `&[crate::record::OrganelleRecord]` — at the call site,
//! pass `registry.all()`.
//!
//! Unknown is a valid outcome: `plan_composition` returns `None` when no
//! chain exists within the depth bound. It never invents a chain — every
//! returned plan passes `verify_plan`.

use crate::record::OrganelleRecord;
use organelle_core::{MutationClass, TypeRef};
use std::collections::VecDeque;

/// Sentinel `from_member` value in `CompositionPlan::wiring`: the input is
/// satisfied directly by an available input rather than by a chain member.
/// In that case `from_out` is the index into the `available` slice passed to
/// `plan_composition`.
pub const EXTERNAL_SOURCE: usize = usize::MAX;

/// A planned composition: ordered members, their wiring, and the composed
/// mutation class (max-rule over members).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositionPlan {
    /// (name, version) of each member, in execution order.
    pub members: Vec<(String, String)>,
    /// (from_member, from_out, to_member, to_in). Member indices refer to
    /// positions in `members`; `from_member == EXTERNAL_SOURCE` means the
    /// input is wired from the available-inputs set (`from_out` = its index).
    pub wiring: Vec<(usize, usize, usize, usize)>,
    /// Max-rule composition of every member's `MutationClass`
    /// (`MutationClass::compose`); the empty plan is `ReadOnly`.
    pub composed_mutation_class: MutationClass,
}

/// Planner bounds. `max_depth` is the maximum number of members in a chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanConfig {
    pub max_depth: usize,
}

impl PlanConfig {
    pub fn new(max_depth: usize) -> Self {
        Self { max_depth }
    }
}

impl Default for PlanConfig {
    fn default() -> Self {
        Self { max_depth: 4 }
    }
}

/// Plan verification failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// A wiring entry refers to a nonexistent member, output, or input.
    BadWire(String),
    /// A wired pair is not type-equal (violates the wire-time rule).
    TypeMismatch { expected: TypeRef, got: TypeRef },
    /// A member input has no wiring entry.
    UnsatisfiedInput { member: usize, input: usize },
    /// The final member does not produce the target output.
    TargetNotProduced,
    /// The composed mutation class does not match the max-rule fold.
    MutationClassMismatch,
    /// A plan member cannot be resolved to a registered record.
    UnknownMember { name: String, version: String },
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanError::BadWire(s) => write!(f, "bad wire: {s}"),
            PlanError::TypeMismatch { expected, got } => {
                write!(f, "type mismatch: expected {expected}, got {got}")
            }
            PlanError::UnsatisfiedInput { member, input } => {
                write!(f, "member {member} input {input} has no wire")
            }
            PlanError::TargetNotProduced => write!(f, "final member does not produce target"),
            PlanError::MutationClassMismatch => write!(f, "composed mutation class mismatch"),
            PlanError::UnknownMember { name, version } => {
                write!(f, "unknown member {name} v{version}")
            }
        }
    }
}

impl std::error::Error for PlanError {}

/// Search for a chain of registered organelles producing `target`.
///
/// `records` is the registry's record list (pass `registry.all()`); the
/// search order is deterministic: candidates are visited in (name, version)
/// order, states are expanded breadth-first, so the first plan found is a
/// shortest chain and ties break deterministically.
///
/// Wiring rule (same as `Composed::wire`): each member input is satisfied by
/// the earliest chain member (in execution order) with a type-equal output,
/// else by the first type-equal available input. A chain whose wiring cannot
/// be verified is never returned.
///
/// Returns `None` when no chain exists within `config.max_depth`. If `target`
/// is already among `available`, returns the empty plan (the identity
/// composition, `ReadOnly`) — nothing needs composing.
pub fn plan_composition(
    target: &TypeRef,
    available: &[TypeRef],
    records: &[OrganelleRecord],
    config: &PlanConfig,
) -> Option<CompositionPlan> {
    if available.contains(target) {
        return Some(CompositionPlan {
            members: Vec::new(),
            wiring: Vec::new(),
            composed_mutation_class: MutationClass::ReadOnly,
        });
    }
    if config.max_depth == 0 {
        return None;
    }

    // Deterministic candidate order: (name, version), matching the frozen
    // find_by_output contract's ordering.
    let mut order: Vec<usize> = (0..records.len()).collect();
    order.sort_by(|&a, &b| {
        (&records[a].name, &records[a].version).cmp(&(&records[b].name, &records[b].version))
    });

    #[derive(Clone)]
    struct SearchState {
        members: Vec<usize>, // indices into `records`
        wiring: Vec<(usize, usize, usize, usize)>,
    }

    let mut queue: VecDeque<SearchState> = VecDeque::from([SearchState {
        members: Vec::new(),
        wiring: Vec::new(),
    }]);

    while let Some(state) = queue.pop_front() {
        if state.members.len() >= config.max_depth {
            continue;
        }
        for &ci in &order {
            let rec = &records[ci];
            // Wire every input of the candidate.
            let mut wires: Vec<(usize, usize, usize, usize)> = Vec::new();
            let mut ok = true;
            for (ii, input) in rec.inputs.iter().enumerate() {
                match find_source(input, &state.members, records, available) {
                    Some((from_pos, from_out)) => {
                        wires.push((from_pos, from_out, state.members.len(), ii));
                    }
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            let mut members = state.members.clone();
            members.push(ci);
            let mut wiring = state.wiring.clone();
            wiring.extend(wires.iter().copied());
            let plan = CompositionPlan {
                members: members
                    .iter()
                    .map(|&i| (records[i].name.clone(), records[i].version.clone()))
                    .collect(),
                wiring,
                composed_mutation_class: members.iter().fold(MutationClass::ReadOnly, |acc, &i| {
                    acc.compose(&records[i].mutation_class)
                }),
            };
            if rec.outputs.contains(target) {
                // Defense in depth: never return an unverifiable plan.
                if verify_plan(&plan, target, available, records).is_ok() {
                    return Some(plan);
                }
                continue;
            }
            queue.push_back(SearchState {
                members,
                wiring: plan.wiring.clone(),
            });
        }
    }
    None
}

/// Find a source for one input: earliest chain member (execution order) with
/// a type-equal output, else the first type-equal available input.
fn find_source(
    input: &TypeRef,
    members: &[usize],
    records: &[OrganelleRecord],
    available: &[TypeRef],
) -> Option<(usize, usize)> {
    for (pos, &mi) in members.iter().enumerate() {
        if let Some(oi) = records[mi].outputs.iter().position(|o| o == input) {
            return Some((pos, oi));
        }
    }
    available
        .iter()
        .position(|a| a == input)
        .map(|ai| (EXTERNAL_SOURCE, ai))
}

/// Verify a plan against the registry records: every member resolves, every
/// input is wired exactly once with type-equal ends, the final member
/// produces `target`, and the composed mutation class matches the max-rule.
pub fn verify_plan(
    plan: &CompositionPlan,
    target: &TypeRef,
    available: &[TypeRef],
    records: &[OrganelleRecord],
) -> Result<(), PlanError> {
    // Resolve members.
    let mut resolved: Vec<&OrganelleRecord> = Vec::with_capacity(plan.members.len());
    for (name, version) in &plan.members {
        let rec = records
            .iter()
            .find(|r| &r.name == name && &r.version == version)
            .ok_or_else(|| PlanError::UnknownMember {
                name: name.clone(),
                version: version.clone(),
            })?;
        resolved.push(rec);
    }

    // Every member input must have exactly one wire; ends must be type-equal.
    for (mi, rec) in resolved.iter().enumerate() {
        for (ii, input) in rec.inputs.iter().enumerate() {
            let wires: Vec<_> = plan
                .wiring
                .iter()
                .filter(|(_, _, to_m, to_i)| *to_m == mi && *to_i == ii)
                .collect();
            if wires.is_empty() {
                return Err(PlanError::UnsatisfiedInput { member: mi, input: ii });
            }
            for (from_m, from_o, _, _) in wires {
                let got: TypeRef = if *from_m == EXTERNAL_SOURCE {
                    available.get(*from_o).cloned().ok_or_else(|| {
                        PlanError::BadWire(format!("external source index {from_o} out of range"))
                    })?
                } else {
                    let src = resolved.get(*from_m).ok_or_else(|| {
                        PlanError::BadWire(format!("member {from_m} out of range"))
                    })?;
                    src.outputs.get(*from_o).cloned().ok_or_else(|| {
                        PlanError::BadWire(format!("member {from_m} output {from_o} out of range"))
                    })?
                };
                if &got != input {
                    return Err(PlanError::TypeMismatch {
                        expected: input.clone(),
                        got,
                    });
                }
            }
        }
    }

    // Final member produces the target (empty plan: target must be available).
    if resolved.is_empty() {
        if !available.contains(target) {
            return Err(PlanError::TargetNotProduced);
        }
    } else if !resolved.last().expect("non-empty").outputs.contains(target) {
        return Err(PlanError::TargetNotProduced);
    }

    // Mutation class matches the max-rule fold.
    let expected = resolved
        .iter()
        .fold(MutationClass::ReadOnly, |acc, r| acc.compose(&r.mutation_class));
    if plan.composed_mutation_class != expected {
        return Err(PlanError::MutationClassMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use organelle_core::Layer;

    fn rec(
        name: &str,
        version: &str,
        inputs: &[&str],
        outputs: &[&str],
        mc: MutationClass,
    ) -> OrganelleRecord {
        OrganelleRecord {
            name: name.to_string(),
            version: version.to_string(),
            layer: Layer::Global,
            mutation_class: mc,
            inputs: inputs.iter().map(|s| TypeRef::global(*s)).collect(),
            outputs: outputs.iter().map(|s| TypeRef::global(*s)).collect(),
            interface_digest: "sha256:00".to_string(),
            artifact_digest: "sha256:00".to_string(),
            source: "test".to_string(),
        }
    }

    fn encode_decode() -> Vec<OrganelleRecord> {
        vec![
            rec("encode", "1.0.0", &["Number"], &["Bits"], MutationClass::ReadOnly),
            rec("decode", "1.0.0", &["Bits"], &["Text"], MutationClass::Write),
        ]
    }

    #[test]
    fn planner_finds_a_two_chain() {
        // decode's Bits input is satisfiable only via encode's Bits output:
        // the direct one-chain route is impossible, so the planner must
        // build the genuine two-chain.
        let records = encode_decode();
        let avail = [TypeRef::global("Number")];
        let plan = plan_composition(
            &TypeRef::global("Text"),
            &avail,
            &records,
            &PlanConfig::default(),
        )
        .expect("chain exists");
        assert_eq!(
            plan.members,
            vec![
                ("encode".to_string(), "1.0.0".to_string()),
                ("decode".to_string(), "1.0.0".to_string()),
            ]
        );
        // encode's Number input comes from the available set...
        assert!(plan.wiring.contains(&(EXTERNAL_SOURCE, 0, 0, 0)));
        // ...and decode's Bits input is wired from encode's Bits output.
        assert!(plan.wiring.contains(&(0, 0, 1, 0)));
        assert_eq!(plan.composed_mutation_class, MutationClass::Write);
        verify_plan(&plan, &TypeRef::global("Text"), &avail, &records).unwrap();
    }

    #[test]
    fn multiple_inputs_wire_from_mixed_sources() {
        // combine needs Number (from a chain member) and Text (available).
        let records = vec![
            rec("number-src", "1.0.0", &[], &["Number"], MutationClass::ReadOnly),
            rec(
                "combine",
                "1.0.0",
                &["Number", "Text"],
                &["Report"],
                MutationClass::ReadOnly,
            ),
        ];
        let avail = [TypeRef::global("Text")];
        let plan = plan_composition(
            &TypeRef::global("Report"),
            &avail,
            &records,
            &PlanConfig::default(),
        )
        .expect("chain exists");
        assert_eq!(plan.members.len(), 2);
        // combine (member 1): input 0 (Number) from number-src (member 0)
        // output 0; input 1 (Text) from available input 0.
        assert!(plan.wiring.contains(&(0, 0, 1, 0)));
        assert!(plan.wiring.contains(&(EXTERNAL_SOURCE, 0, 1, 1)));
        verify_plan(&plan, &TypeRef::global("Report"), &avail, &records).unwrap();
    }

    #[test]
    fn no_chain_returns_none() {
        let records = vec![rec(
            "double",
            "1.0.0",
            &["Number"],
            &["Number"],
            MutationClass::ReadOnly,
        )];
        let plan = plan_composition(
            &TypeRef::global("Text"),
            &[TypeRef::global("Number")],
            &records,
            &PlanConfig::default(),
        );
        assert!(plan.is_none(), "no producer of Text exists");
    }

    #[test]
    fn max_depth_is_respected() {
        // Genuine 5-link chain: step i consumes T_i and produces T_{i+1},
        // so no shortcut exists — only the full chain reaches T_5.
        let t: Vec<String> = (0..=5).map(|i| format!("T{i}")).collect();
        let tr: Vec<&str> = t.iter().map(|s| s.as_str()).collect();
        let records: Vec<OrganelleRecord> = (0..5)
            .map(|i| {
                rec(
                    &format!("step{i}"),
                    "1.0.0",
                    &[tr[i]],
                    &[tr[i + 1]],
                    MutationClass::ReadOnly,
                )
            })
            .collect();
        let target = TypeRef::global("T5");
        let avail = [TypeRef::global("T0")];
        assert!(plan_composition(&target, &avail, &records, &PlanConfig::default()).is_none());
        let deep =
            plan_composition(&target, &avail, &records, &PlanConfig::new(5)).expect("depth 5 suffices");
        assert_eq!(deep.members.len(), 5);
        verify_plan(&deep, &target, &avail, &records).unwrap();
    }

    #[test]
    fn mutation_class_follows_max_rule() {
        let records = vec![
            rec("pure", "1.0.0", &[], &["Number"], MutationClass::ReadOnly),
            rec("net", "1.0.0", &["Number"], &["Text"], MutationClass::ExternalEffect),
        ];
        let plan = plan_composition(
            &TypeRef::global("Text"),
            &[],
            &records,
            &PlanConfig::default(),
        )
        .expect("chain exists");
        assert_eq!(plan.composed_mutation_class, MutationClass::ExternalEffect);
    }

    #[test]
    fn candidate_order_is_deterministic() {
        // Two producers of Text; (name, version)-earlier one must win.
        let records = vec![
            rec("zebra", "1.0.0", &[], &["Text"], MutationClass::ReadOnly),
            rec("alpha", "1.0.0", &[], &["Text"], MutationClass::ReadOnly),
        ];
        let plan = plan_composition(
            &TypeRef::global("Text"),
            &[],
            &records,
            &PlanConfig::default(),
        )
        .expect("chain exists");
        assert_eq!(plan.members, vec![("alpha".to_string(), "1.0.0".to_string())]);
    }

    #[test]
    fn target_already_available_yields_empty_plan() {
        let records = encode_decode();
        let plan = plan_composition(
            &TypeRef::global("Text"),
            &[TypeRef::global("Text")],
            &records,
            &PlanConfig::default(),
        )
        .expect("identity plan");
        assert!(plan.members.is_empty());
        assert_eq!(plan.composed_mutation_class, MutationClass::ReadOnly);
    }

    #[test]
    fn verify_rejects_type_mismatch() {
        let records = encode_decode();
        let bad = CompositionPlan {
            members: vec![("decode".to_string(), "1.0.0".to_string())],
            // Wire decode's Bits input from available Text: wrong type.
            wiring: vec![(EXTERNAL_SOURCE, 0, 0, 0)],
            composed_mutation_class: MutationClass::Write,
        };
        let err = verify_plan(
            &bad,
            &TypeRef::global("Text"),
            &[TypeRef::global("Text")],
            &records,
        )
        .unwrap_err();
        assert!(matches!(err, PlanError::TypeMismatch { .. }));
    }

    #[test]
    fn verify_rejects_unsatisfied_input() {
        let records = encode_decode();
        let bad = CompositionPlan {
            members: vec![("decode".to_string(), "1.0.0".to_string())],
            wiring: vec![],
            composed_mutation_class: MutationClass::Write,
        };
        let err = verify_plan(
            &bad,
            &TypeRef::global("Text"),
            &[TypeRef::global("Number")],
            &records,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            PlanError::UnsatisfiedInput { member: 0, input: 0 }
        ));
    }
}
