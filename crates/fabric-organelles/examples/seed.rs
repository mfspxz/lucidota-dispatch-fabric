//! Seed the organelle registry with the fabric's own crates.
//!
//! The fabric registers itself: every crate becomes an organelle with a
//! typed interface (inputs/outputs as ontology TypeRefs), so the dispatcher
//! can discover worker organelles from its own registry.
//!
//! Usage: cargo run --offline -p fabric-organelles --example seed -- <out-path>

use fabric_organelles::record::compute_interface_digest;
use fabric_organelles::{OrganelleRecord, OrganelleRegistry};
use organelle_core::{Layer, MutationClass, TypeRef};

fn t(layer: Layer, name: &str) -> TypeRef {
    TypeRef::new(layer, name)
}

fn rec(
    name: &str,
    layer: Layer,
    mc: MutationClass,
    inputs: Vec<TypeRef>,
    outputs: Vec<TypeRef>,
    source: &str,
) -> OrganelleRecord {
    let mut r = OrganelleRecord::new(
        name,
        "0.1.0",
        layer,
        mc,
        inputs,
        outputs,
        // artifact digest: sha256 of the crate dir is the honest value;
        // the seeder stamps "workspace:dispatch-fabric" as the source family
        // and leaves per-artifact digests to the build that produces them.
        "pending:build-artifact",
        source,
    );
    r.interface_digest = compute_interface_digest(&r);
    r
}

fn main() {
    let out = std::env::args().nth(1).expect("usage: seed <out-path>");
    let g = Layer::Global;
    let d = Layer::Domain;
    let mut reg = OrganelleRegistry::new();

    let organelles = vec![
        rec(
            "organelle-core", g, MutationClass::ReadOnly,
            vec![t(g, "TypedValue")], vec![t(g, "TypedValue")],
            "workspace:organelle/core",
        ),
        rec(
            "fabric-types", g, MutationClass::ReadOnly,
            vec![], vec![t(d, "TypeDefs")],
            "workspace:dispatch-fabric/crates/fabric-types",
        ),
        rec(
            "fabric-spec", d, MutationClass::ReadOnly,
            vec![t(d, "SpecText")], vec![t(d, "Manifest")],
            "workspace:dispatch-fabric/crates/fabric-spec",
        ),
        rec(
            "fabric-registry", d, MutationClass::Write,
            vec![t(d, "EndpointConfig")], vec![t(d, "EndpointHandle")],
            "workspace:dispatch-fabric/crates/fabric-registry",
        ),
        rec(
            "fabric-adapters", d, MutationClass::ExternalEffect,
            vec![t(d, "InferenceRequest")], vec![t(d, "InferenceResponse")],
            "workspace:dispatch-fabric/crates/fabric-adapters",
        ),
        rec(
            "fabric-quota", d, MutationClass::Write,
            vec![t(d, "UsageReport")], vec![t(d, "AdmissionDecision")],
            "workspace:dispatch-fabric/crates/fabric-quota",
        ),
        rec(
            "fabric-graph", d, MutationClass::ReadOnly,
            vec![t(d, "TaskList")], vec![t(d, "TaskGraph")],
            "workspace:dispatch-fabric/crates/fabric-graph",
        ),
        rec(
            "fabric-dispatch", d, MutationClass::Write,
            vec![t(d, "DispatchCandidate")], vec![t(d, "DispatchEnvelope")],
            "workspace:dispatch-fabric/crates/fabric-dispatch",
        ),
        rec(
            "fabric-workspace", d, MutationClass::ExternalEffect,
            vec![t(d, "TaskAttempt")], vec![t(d, "Workspace")],
            "workspace:dispatch-fabric/crates/fabric-workspace",
        ),
        rec(
            "fabric-records", d, MutationClass::Write,
            vec![t(d, "Event")], vec![t(d, "Receipt")],
            "workspace:dispatch-fabric/crates/fabric-records",
        ),
        rec(
            "fabric-verify", d, MutationClass::ExternalEffect,
            vec![t(d, "Artifact")], vec![t(d, "Verdict")],
            "workspace:dispatch-fabric/crates/fabric-verify",
        ),
        rec(
            "fabric-organelles", g, MutationClass::ReadOnly,
            vec![t(d, "OrganelleQuery")], vec![t(d, "OrganelleRecord")],
            "workspace:dispatch-fabric/crates/fabric-organelles",
        ),
        rec(
            "fabric-driver", d, MutationClass::ExternalEffect,
            vec![t(d, "WorkOrder")], vec![t(d, "DriverReport")],
            "workspace:dispatch-fabric/crates/fabric-driver",
        ),
    ];

    for o in organelles {
        reg.register(o).expect("registration must succeed");
    }
    fabric_organelles::persist::save_registry(reg.all(), std::path::Path::new(&out))
        .expect("save must succeed");
    println!("seeded {} organelles -> {}", reg.len(), out);
}
