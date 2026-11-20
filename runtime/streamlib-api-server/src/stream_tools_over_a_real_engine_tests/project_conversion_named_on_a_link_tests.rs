// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A conversion a project's own Python node states is named, never placed: a link only it
//! bridges is refused with `link_needs_a_module_conversion` and the `transact` that places it,
//! and `check_link` answers the same. The node types are the `every-pin-kind` sample's, as its
//! interpreter describes them: `SweepAt` gives the sample's named type `Sweep`, `SweepPosition`
//! states `conversion=True` from a `Sweep` to a Float, and `Warmth` takes a Float on `base`.

use super::*;

const SWEEP_AT_TYPE: &str = "nodes.sweep:SweepAt";
const SWEEP_POSITION_TYPE: &str = "nodes.sweep:SweepPosition";
const WARMTH_TYPE: &str = "nodes.beat:Warmth";

fn a_sweep() -> PinType {
    serde_json::from_value(json!({ "type": "named", "named_pin_type": "nodes.sweep:Sweep" }))
        .expect("the sample's named type")
}

fn a_float() -> PinType {
    PinType::Float { exact_size: None }
}

/// A loaded stream `main` of a project, holding a `sweep` of `SweepAt` and a `warmth` of
/// `Warmth`, its stream resolving the sample's three node types as its interpreter described
/// them, none declared by an installed module; its id, the project and the two nodes' UUIDs.
async fn a_stream_holding_a_sweep_and_a_warmth(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
) -> (String, ProjectWithGraphDocuments, String, String) {
    let project = ProjectWithGraphDocuments::holding(json!({
        "stream": "main", "nodes": [], "links": [],
    }));
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let loaded = engine
        .engine
        .loaded_stream_of_the_id(&stream_id)
        .expect("the stream is loaded");
    let node_types = loaded.node_types_this_stream_resolves();
    for descriptor in [
        a_python_function_node_type(
            SWEEP_AT_TYPE,
            vec![
                a_typed_port("position", a_float()),
                a_typed_port("width", a_float()),
            ],
            vec![a_typed_port("result", a_sweep())],
        ),
        a_python_function_node_type(
            SWEEP_POSITION_TYPE,
            vec![a_typed_port("sweep", a_sweep())],
            vec![a_typed_port("result", a_float())],
        )
        .stating_a_conversion(),
        a_python_function_node_type(
            WARMTH_TYPE,
            vec![a_typed_port("base", a_float())],
            vec![a_typed_port("result", a_float())],
        ),
    ] {
        node_types
            .register_a_described_type_that_runs_no_node_for_a_test(descriptor, None)
            .expect("the stream resolves the sample's node type");
    }
    let stream = project.stream_asset_of("main");
    let added = engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({
                "author": an_agent_named("alice"),
                "operations": [
                    { "operation": "add_node", "stream": stream, "type": SWEEP_AT_TYPE,
                      "name": "sweep" },
                    { "operation": "add_node", "stream": stream, "type": WARMTH_TYPE,
                      "name": "warmth" },
                ],
            }),
        )
        .await
        .unwrap_or_else(|refusal| panic!("the two nodes are placed: {refusal:?}"));
    let the_uuid_of = |index: usize| {
        added["added_nodes"][index]["node"]
            .as_str()
            .expect("an added node's UUID")
            .to_owned()
    };
    let (sweep, warmth) = (the_uuid_of(0), the_uuid_of(1));
    (stream_id, project, sweep, warmth)
}

/// A link from a `Sweep` into a Float is refused with `link_needs_a_module_conversion`, naming
/// the project's `SweepPosition`, and its `suggested_call` is the `transact` placing it between
/// the two ends; `check_link` answers that same refusal and call, and nothing is placed.
///
/// Mental-revert: refusing a conversion no installed module declares refuses `SweepPosition` as
/// the stream registers it, and the stream has no conversion to name.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_only_a_projects_conversion_bridges_is_refused_naming_it_with_the_call_placing_it() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let (stream_id, project, sweep, warmth) = a_stream_holding_a_sweep_and_a_warmth(&engine).await;

    engine
        .the_call_placing_the_conversion_a_refused_link_names(
            &stream_id,
            &json!({
                "stream": project.stream_asset_of("main"),
                "source": { "node": sweep, "port": "result" },
                "target": { "node": warmth, "port": "base" },
            }),
            ConversionToPlace {
                node_type: SWEEP_POSITION_TYPE,
                input_port: "sweep",
                output_port: "result",
            },
        )
        .await;

    assert_eq!(
        project.the_graph_document_of("main")["nodes"]
            .as_array()
            .map(Vec::len),
        Some(2),
        "neither the refused connect nor the check placed a node"
    );
}

/// `list_pin_types` lists the conversion a project's node type states beside the built-in ones,
/// read from the conversions the project's loaded stream resolves.
///
/// Mental-revert: listing the native conversions alone leaves `SweepPosition` out of the named
/// type's row.
#[tokio::test(flavor = "multi_thread")]
async fn a_conversion_a_projects_loaded_stream_resolves_is_listed_among_the_pin_types() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let (_, project, _, _) = a_stream_holding_a_sweep_and_a_warmth(&engine).await;

    let listed = engine
        .the_answer_or_the_refusal_of(
            "list_pin_types",
            json!({ "project_directory": project.path() }),
        )
        .await
        .unwrap_or_else(|refusal| panic!("the pin types are listed: {refusal:?}"));

    let converts_to: Vec<Value> = listed["pin_types"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|row| row["converts_to"].as_array().cloned().unwrap_or_default())
        .collect();
    assert!(
        converts_to
            .iter()
            .any(|conversion| conversion["node_type"] == SWEEP_POSITION_TYPE),
        "{listed}"
    );
}
