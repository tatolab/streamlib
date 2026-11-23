// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link closing a loop of function nodes, where `connect` judges a link by the node types of
//! its level before anything is written, is refused as that judgement refuses it before the
//! `PreviousValue` node is added; one it takes is closed through that node, and a retry under
//! its key names the node as inserted to close the loop.

use streamlib::sdk::descriptors::{
    FunctionNodeLanguage, NodeClassImportPath, NodeClassShortName, NodeDescriptor, NodeKind,
    PinDeclaration, PinType, PortDescriptor, PortFamily, PortFamilyDeclaration,
};
use streamlib_runtime_client_contract::local_api_wire_contract::ControlOperationRefusalCode;

use super::link_checked_before_it_is_made_tests::register_the_link_check_node_types_once;
use super::*;

const TYPED_LOOP_TYPE: &str = "loop_check_test:TypedLoop";
const FIRST_UUID: &str = "a1a1a1a1-a1a1-4a1a-8a1a-a1a1a1a1a1a1";
const SECOND_UUID: &str = "b2b2b2b2-b2b2-4b2b-8b2b-b2b2b2b2b2b2";
const SPARE_UUID: &str = "c3c3c3c3-c3c3-4c3c-8c3c-c3c3c3c3c3c3";

/// A typed port of `pin_type` named `name`, delivered newest when it is an input.
fn a_typed_port(name: &str, pin_type: PinType, is_an_input: bool) -> PortDescriptor {
    let port = PortDescriptor::new(
        name,
        "",
        true,
        PortFamilyDeclaration::of_family(
            PortFamily::carrying(&pin_type).unwrap_or(PortFamily::Records),
        ),
    )
    .with_pin_declaration(PinDeclaration::exactly(pin_type));
    if is_an_input {
        port.with_delivery_profile("newest")
    } else {
        port
    }
}

/// Register, once per test binary, the link check node types, `PreviousValue` among them, and a
/// function node type taking String `text` and Float `number` and giving Float `level`.
fn register_the_loop_check_node_types_once() {
    register_the_link_check_node_types_once();
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        let node_type = NodeClassImportPath::new(TYPED_LOOP_TYPE).expect("a node type");
        let mut descriptor = NodeDescriptor::new(
            NodeClassShortName::new("TypedLoop").expect("a short name"),
            node_type,
            "a typed function node a loop check judges",
        )
        .with_input(a_typed_port("text", PinType::String {}, true))
        .with_input(a_typed_port(
            "number",
            PinType::Float { exact_size: None },
            true,
        ))
        .with_output(a_typed_port(
            "level",
            PinType::Float { exact_size: None },
            false,
        ));
        descriptor.node_kind = NodeKind::Function {
            language: FunctionNodeLanguage::Python,
        };
        NODE_REGISTRY
            .register_descriptor_only(descriptor)
            .expect("the typed loop node type is registered here alone");
    });
}

/// A project whose stream `main` holds two typed function nodes, `second`'s `level` feeding
/// `first`'s `number`, and a spare node of the same type.
fn a_project_whose_main_stream_holds_two_typed_function_nodes() -> ProjectWithGraphDocuments {
    register_the_loop_check_node_types_once();
    let node = |id: &str, name: &str| json!({ "id": id, "name": name, "type": TYPED_LOOP_TYPE });
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [node(FIRST_UUID, "first"), node(SECOND_UUID, "second"), node(SPARE_UUID, "spare")],
        "links": [
            { "source": { "node": SECOND_UUID, "port": "level" },
              "target": { "node": FIRST_UUID, "port": "number" } },
        ],
    }))
}

/// On a stream an earlier operation of the transaction has take its edits once the documents
/// hold them, a link closing the loop whose pin types refuse it, a Float into a String, is
/// refused before anything is written, and no `PreviousValue` node is added.
///
/// Mental-revert: returning the `PreviousValue` node before judging the link commits the node
/// and both its links.
#[tokio::test(flavor = "multi_thread")]
async fn a_loop_closing_link_on_a_stream_taking_its_edits_later_is_judged_before_commit() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_two_typed_function_nodes();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let saved_before = project.the_graph_document_of("main");

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({ "operations": [
                { "operation": "collapse_selection_into_subgraph", "stream": stream_asset,
                  "nodes": [SPARE_UUID], "name": "sub" },
                { "operation": "connect", "stream": stream_asset,
                  "source": { "node": FIRST_UUID, "port": "level" },
                  "target": { "node": SECOND_UUID, "port": "text" } },
            ], "author": an_agent_named("alice") }),
        )
        .await
        .expect_err("a Float does not connect into a String");

    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::LinkPinTypesDoNotConnect,
        "{refusal:?}"
    );
    assert_eq!(refusal.refused_operation_index, Some(1), "{refusal:?}");
    assert_eq!(project.the_graph_document_of("main"), saved_before);
}

/// Inside a placed stream, a link closing the loop that its pin types refuse is refused before
/// anything is written; one they take is closed through a `PreviousValue` node, and the same
/// `connect` sent again under its key names that node as inserted to close the loop.
///
/// Mental-revert: keeping the inserted node under the UUID it is drawn under, while a retry reads
/// the node by its UUID in the placed stream's document, answers the retry without
/// `inserted_because`.
#[tokio::test(flavor = "multi_thread")]
async fn a_loop_closing_link_inside_a_placed_stream_is_judged_and_answered_again_alike() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_two_typed_function_nodes();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let collapsed = engine
        .the_answer_or_the_refusal_of(
            "collapse_selection_into_subgraph",
            json!({ "stream": stream_asset, "nodes": [FIRST_UUID, SECOND_UUID],
                    "name": "sub", "author": an_agent_named("alice") }),
        )
        .await
        .expect("both function nodes collapse into `sub`");
    let subgraph = collapsed["added_nodes"][0]["node"]
        .as_str()
        .expect("the subgraph node")
        .to_owned();
    let drawn = |name: &str| {
        let held = the_node_named(&project.the_graph_document_of("sub"), name);
        the_uuid_a_held_node_takes_under(&subgraph, held["id"].as_str().unwrap())
    };
    let (first_drawn, second_drawn) = (drawn("first"), drawn("second"));
    let sub_before = project.the_graph_document_of("sub");
    let closing = |input: &str| {
        json!({ "stream": stream_asset, "level": [subgraph],
                "source": { "node": first_drawn, "port": "level" },
                "target": { "node": second_drawn, "port": input },
                "author": an_agent_named("alice"), "idempotency_key": format!("close-{input}") })
    };

    let refusal = engine
        .the_answer_or_the_refusal_of("connect", closing("text"))
        .await
        .expect_err("a Float does not connect into a String");
    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::LinkPinTypesDoNotConnect,
        "{refusal:?}"
    );
    assert_eq!(project.the_graph_document_of("sub"), sub_before);

    let closed = engine
        .the_answer_or_the_refusal_of("connect", closing("number"))
        .await
        .unwrap_or_else(|refusal| panic!("the loop closes through a node: {refusal:?}"));
    assert_eq!(
        closed["added_nodes"][0]["inserted_because"], "closes_a_loop",
        "{closed}"
    );
    let sent_again = engine
        .the_answer_or_the_refusal_of("connect", closing("number"))
        .await
        .unwrap_or_else(|refusal| panic!("the connect sent again answers: {refusal:?}"));
    assert_eq!(
        sent_again["transaction"], closed["transaction"],
        "{sent_again}"
    );
    assert_eq!(
        sent_again["added_nodes"][0]["inserted_because"], "closes_a_loop",
        "{sent_again}"
    );
    assert_eq!(
        sent_again["added_nodes"][0]["node_type"], closed["added_nodes"][0]["node_type"],
        "{sent_again}"
    );
}
