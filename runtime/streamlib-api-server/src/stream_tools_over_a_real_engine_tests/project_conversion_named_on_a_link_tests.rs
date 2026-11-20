// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A conversion a project's own Python node states is named, never placed: a link only it
//! bridges is refused with `link_needs_a_module_conversion` and the `transact` that places it,
//! and `check_link` answers the same. The node types are the `every-pin-kind` sample's, as its
//! interpreter describes them: `SweepAt` gives the sample's named type `Sweep`, `SweepPosition`
//! states `conversion=True` from a `Sweep` to a Float, and `Warmth` takes a Float on `base`.

use streamlib::sdk::descriptors::{
    FunctionNodeLanguage, NodeClassImportPath, NodeClassShortName, NodeKind, PinDeclaration,
    PinType, PortDescriptor, PortFamily, PortFamilyDeclaration,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, SuggestedOperationCall,
};

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

fn a_port_of(name: &str, pin_type: PinType) -> PortDescriptor {
    PortDescriptor::new(
        name,
        "",
        true,
        PortFamilyDeclaration::of_family(
            PortFamily::carrying(&pin_type).unwrap_or(PortFamily::Records),
        ),
    )
    .with_delivery_profile("newest")
    .with_pin_declaration(PinDeclaration::exactly(pin_type))
}

/// The Python function node type `node_type` taking `inputs` and giving `outputs`.
fn a_python_function_node_type(
    node_type: &str,
    inputs: Vec<PortDescriptor>,
    outputs: Vec<PortDescriptor>,
) -> NodeDescriptor {
    let node_type = NodeClassImportPath::new(node_type).expect("a node type");
    let short_name = node_type.as_str().rsplit(':').next().expect("a short name");
    let mut descriptor = NodeDescriptor::new(
        NodeClassShortName::new(short_name).expect("a short name"),
        node_type,
        "a node type of the every-pin-kind sample",
    );
    descriptor.node_kind = NodeKind::Function {
        language: FunctionNodeLanguage::Python,
    };
    descriptor.inputs = inputs;
    descriptor.outputs = outputs;
    descriptor
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
                a_port_of("position", a_float()),
                a_port_of("width", a_float()),
            ],
            vec![a_port_of("result", a_sweep())],
        ),
        a_python_function_node_type(
            SWEEP_POSITION_TYPE,
            vec![a_port_of("sweep", a_sweep())],
            vec![a_port_of("result", a_float())],
        )
        .stating_a_conversion(),
        a_python_function_node_type(
            WARMTH_TYPE,
            vec![a_port_of("base", a_float())],
            vec![a_port_of("result", a_float())],
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
    let link = json!({
        "stream": project.stream_asset_of("main"),
        "source": { "node": sweep, "port": "result" },
        "target": { "node": warmth, "port": "base" },
    });

    let mut connect = link.clone();
    connect["author"] = an_agent_named("alice");
    let refusal = engine
        .the_answer_or_the_refusal_of("connect", connect)
        .await
        .expect_err("connect never places a project's conversion");

    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::LinkNeedsAModuleConversion,
        "{refusal:?}"
    );
    assert!(
        refusal
            .runtime_words
            .contains(&format!("`{SWEEP_POSITION_TYPE}`")),
        "{refusal:?}"
    );
    let Some(SuggestedOperationCall { operation, params }) = refusal.suggested_call.clone() else {
        panic!("the refusal suggests the call that places the conversion: {refusal:?}");
    };
    assert_eq!(operation, "transact");
    assert_eq!(
        Value::Object(params),
        json!({ "operations": [
            { "operation": "add_node", "stream": stream_id,
              "type": SWEEP_POSITION_TYPE, "as": "conversion" },
            { "operation": "connect", "stream": stream_id,
              "source": { "node": sweep, "port": "result" },
              "target": { "node": "conversion", "port": "sweep" } },
            { "operation": "connect", "stream": stream_id,
              "source": { "node": "conversion", "port": "result" },
              "target": { "node": warmth, "port": "base" } },
        ]})
    );

    let checked = engine
        .the_answer_or_the_refusal_of("check_link", link)
        .await
        .unwrap_or_else(|refusal| panic!("check_link answers: {refusal:?}"));
    assert_eq!(checked["outcome"], "refused", "{checked}");
    assert_eq!(
        checked["refusal"]["refusal_code"], "link_needs_a_module_conversion",
        "{checked}"
    );
    assert_eq!(
        checked["refusal"]["suggested_call"],
        serde_json::to_value(&refusal.suggested_call).unwrap(),
        "check_link suggests the call connect suggests"
    );
    assert_eq!(
        project.the_graph_document_of("main")["nodes"]
            .as_array()
            .map(Vec::len),
        Some(2),
        "neither the refused connect nor the check placed a node"
    );
}
