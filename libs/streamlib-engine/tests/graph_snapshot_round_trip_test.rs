// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Round-trip equivalence for the graph snapshot save/load API.
//!
//! The contract: an imperative build → save → load (into a fresh
//! runtime) → save-again produces byte-equivalent JSON. The save
//! side has no graph state of its own to lean on; if any field
//! drifts across the round-trip, the second save diverges and this
//! test catches it.
//!
//! Also locks:
//! - Deterministic alias regeneration (two same-type processors
//!   become `<short>` and `<short>_2` in node-iteration order).
//! - `display_name` override survives load → save.
//! - Pipeline `name` survives load → save without caller bookkeeping.

use serial_test::serial;
use streamlib::sdk::descriptors::{
    Org, Package, PortDescriptor, PortSchemaSpec, ProcessorDescriptor, SchemaIdent, SemVer,
    TypeName,
};
use streamlib::sdk::graph_snapshot::GraphSnapshot;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processors::{ProcessorSpec, PROCESSOR_REGISTRY};
use streamlib::sdk::runtime::Runner;

fn ident(short: &str) -> SchemaIdent {
    SchemaIdent::new(
        Org::new("tatolab").unwrap(),
        Package::new("snapshot-test").unwrap(),
        TypeName::new(short).unwrap(),
        SemVer::new(1, 0, 0),
    )
}

/// Register a descriptor-only processor type with two `Any`-typed
/// ports — enough to satisfy `add_processor`'s port-info lookup and
/// `connect`'s port existence check. Idempotent under `serial_test`.
fn register_test_type(short: &str, input: &str, output: &str) -> SchemaIdent {
    let id = ident(short);
    let descriptor = ProcessorDescriptor::new(id.clone(), "snapshot round-trip test")
        .with_input(PortDescriptor::new(
            input,
            "",
            PortSchemaSpec::Any,
            false,
        ))
        .with_output(PortDescriptor::new(
            output,
            "",
            PortSchemaSpec::Any,
            false,
        ));
    // Idempotent across `serial_test` runs — second register returns
    // `Error::Configuration("Processor 'X' already registered")` which
    // we ignore.
    let _ = PROCESSOR_REGISTRY.register_descriptor_only(descriptor);
    id
}

#[test]
#[serial]
fn imperative_build_save_load_save_is_byte_equivalent() {
    let cam = register_test_type("CameraProc", "_unused_in", "video");
    let dsp = register_test_type("DisplayProc", "video_in", "_unused_out");

    // First runtime — imperative build.
    let r1 = Runner::new().unwrap();
    r1.set_pipeline_name(Some("rt-fixture".to_string()));
    let cam_id = r1
        .add_processor(ProcessorSpec::new(cam.clone(), serde_json::json!({})))
        .unwrap();
    let dsp_id = r1
        .add_processor(ProcessorSpec::new(
            dsp.clone(),
            serde_json::json!({"width": 1920, "height": 1080}),
        ))
        .unwrap();
    r1.connect(
        OutputLinkPortRef::new(&cam_id, "video"),
        InputLinkPortRef::new(&dsp_id, "video_in"),
    )
    .unwrap();

    let snap1 = r1.save_graph_snapshot().unwrap();
    let json1 = snap1.to_json_string().unwrap();

    // Second runtime — load the snapshot from disk-equivalent JSON,
    // then save again. The byte-equivalence holds across this cycle
    // because (a) the snapshot carries everything load needs and (b)
    // save regenerates aliases deterministically from each node's
    // PascalCase short name in insertion order.
    let r2 = Runner::new().unwrap();
    let snap_from_json = GraphSnapshot::from_json_str(&json1).unwrap();
    r2.load_graph_snapshot(&snap_from_json).unwrap();
    let snap2 = r2.save_graph_snapshot().unwrap();
    let json2 = snap2.to_json_string().unwrap();

    assert_eq!(
        json1, json2,
        "second save must byte-equal the first save"
    );

    // Pipeline name survived the round-trip without explicit threading.
    assert_eq!(snap2.name.as_deref(), Some("rt-fixture"));
}

#[test]
#[serial]
fn save_side_regenerates_aliases_on_collision() {
    let cam = register_test_type("CameraProc", "_unused_in", "video");

    let runtime = Runner::new().unwrap();
    runtime
        .add_processor(ProcessorSpec::new(cam.clone(), serde_json::json!({})))
        .unwrap();
    runtime
        .add_processor(ProcessorSpec::new(cam.clone(), serde_json::json!({})))
        .unwrap();
    runtime
        .add_processor(ProcessorSpec::new(cam.clone(), serde_json::json!({})))
        .unwrap();

    let snap = runtime.save_graph_snapshot().unwrap();
    let aliases: Vec<&str> = snap
        .processors
        .iter()
        .map(|p| p.alias.as_str())
        .collect();
    assert_eq!(
        aliases,
        vec!["cameraProc", "cameraProc_2", "cameraProc_3"],
        "collision suffixes must be deterministic in node-iteration order"
    );

    // Mentally revert the collision-suffix logic (always use the base
    // alias) and this assertion fails on the duplicate-alias rejection
    // path — `validate()` rejects duplicate aliases in `load_graph_snapshot`,
    // so round-trip would error.
    let json = snap.to_json_string().unwrap();
    let r2 = Runner::new().unwrap();
    r2.load_graph_snapshot(&GraphSnapshot::from_json_str(&json).unwrap())
        .unwrap();
}

#[test]
#[serial]
fn display_name_override_round_trips() {
    let cam = register_test_type("CameraProc", "_unused_in", "video");

    let r1 = Runner::new().unwrap();
    r1.add_processor(
        ProcessorSpec::new(cam.clone(), serde_json::json!({}))
            .with_display_name("Front-Left Camera"),
    )
    .unwrap();
    // And one without an override — saved snapshot must omit
    // `display_name` for this node so a load → save cycle stays
    // byte-stable.
    r1.add_processor(ProcessorSpec::new(cam.clone(), serde_json::json!({})))
        .unwrap();

    let snap1 = r1.save_graph_snapshot().unwrap();
    assert_eq!(
        snap1.processors[0].display_name.as_deref(),
        Some("Front-Left Camera"),
        "explicit display_name must serialize"
    );
    assert!(
        snap1.processors[1].display_name.is_none(),
        "default display_name must NOT serialize"
    );

    // Round-trip the explicit override back through a fresh runtime.
    let r2 = Runner::new().unwrap();
    r2.load_graph_snapshot(&snap1).unwrap();
    let snap2 = r2.save_graph_snapshot().unwrap();
    assert_eq!(snap1, snap2, "round-trip must preserve display_name shape");
}

#[test]
#[serial]
fn empty_graph_round_trips() {
    let r1 = Runner::new().unwrap();
    let snap1 = r1.save_graph_snapshot().unwrap();
    assert!(snap1.processors.is_empty());
    assert!(snap1.connections.is_empty());
    assert!(snap1.name.is_none());

    let r2 = Runner::new().unwrap();
    r2.load_graph_snapshot(&snap1).unwrap();
    let snap2 = r2.save_graph_snapshot().unwrap();
    assert_eq!(snap1, snap2);
}
