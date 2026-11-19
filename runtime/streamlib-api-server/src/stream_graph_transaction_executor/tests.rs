// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashSet;
use std::path::Path;

use serde_json::json;
use streamlib_project_repository::{
    PREVIOUS_VALUE_NODE_TYPE, StreamGraphDocument, StreamGraphDocumentLink,
    StreamGraphDocumentLinkEnd, StreamGraphOperation,
};
use uuid::Uuid;

use super::request_to_graph_operation::{
    the_link_closes_a_loop_of_function_nodes_no_previous_value_node_is_in,
    the_previous_value_node_added_on_the_link,
};
use super::the_module_a_project_file_is_imported_as;

fn a_link_between(source: Uuid, target: Uuid) -> StreamGraphDocumentLink {
    StreamGraphDocumentLink {
        source: StreamGraphDocumentLinkEnd {
            node: source,
            port: "result".to_owned(),
        },
        target: StreamGraphDocumentLinkEnd {
            node: target,
            port: "x".to_owned(),
        },
    }
}

/// A document of an `add` node at `[0, 0]` linked into a `scale` node at `[200, 100]`, whose
/// type is `type_of_the_scale_node`.
fn add_linked_into_scale(type_of_the_scale_node: &str) -> (StreamGraphDocument, Uuid, Uuid) {
    let (add, scale) = (Uuid::new_v4(), Uuid::new_v4());
    let document = StreamGraphDocument::from_value(json!({
        "asset": Uuid::new_v4(),
        "nodes": [
            { "id": add, "name": "add", "type": "maths:Add",
              "studio": { "position": [0, 0] } },
            { "id": scale, "name": "scale", "type": type_of_the_scale_node,
              "studio": { "position": [200, 100] } },
        ],
        "links": [{
            "source": { "node": add, "port": "result" },
            "target": { "node": scale, "port": "x" },
        }],
    }))
    .unwrap();
    (document, add, scale)
}

#[test]
fn a_link_closing_a_loop_of_function_nodes_is_carried_through_a_previous_value_node() {
    let (document, add, scale) = add_linked_into_scale("maths:Scale");
    let closing_link = a_link_between(scale, add);
    assert!(
        the_link_closes_a_loop_of_function_nodes_no_previous_value_node_is_in(
            &document,
            &closing_link,
            &|| Ok(HashSet::from([add, scale])),
        )
        .unwrap()
    );

    let StreamGraphOperation::AddNode { node, links } =
        the_previous_value_node_added_on_the_link(&document, closing_link)
    else {
        panic!("the loop closes by adding a node");
    };
    assert_eq!(node.node_type, PREVIOUS_VALUE_NODE_TYPE);
    assert_eq!(
        node.studio
            .position
            .map(|pair| pair.map(|number| number.as_f64())),
        Some([Some(100.0), Some(50.0)])
    );
    assert_eq!(
        links,
        [
            StreamGraphDocumentLink {
                source: StreamGraphDocumentLinkEnd {
                    node: scale,
                    port: "result".to_owned(),
                },
                target: StreamGraphDocumentLinkEnd {
                    node: node.id,
                    port: "value".to_owned(),
                },
            },
            StreamGraphDocumentLink {
                source: StreamGraphDocumentLinkEnd {
                    node: node.id,
                    port: "result".to_owned(),
                },
                target: StreamGraphDocumentLinkEnd {
                    node: add,
                    port: "x".to_owned(),
                },
            },
        ]
    );
}

#[test]
fn a_function_node_linked_into_itself_closes_a_loop() {
    let (document, add, _) = add_linked_into_scale("maths:Scale");
    assert!(
        the_link_closes_a_loop_of_function_nodes_no_previous_value_node_is_in(
            &document,
            &a_link_between(add, add),
            &|| Ok(HashSet::from([add])),
        )
        .unwrap()
    );
}

#[test]
fn a_loop_through_a_node_that_is_no_function_node_is_linked_as_asked() {
    let (document, add, scale) = add_linked_into_scale("nodes.scale:ScaleNode");
    assert!(
        !the_link_closes_a_loop_of_function_nodes_no_previous_value_node_is_in(
            &document,
            &a_link_between(scale, add),
            &|| Ok(HashSet::from([add])),
        )
        .unwrap()
    );
}

#[test]
fn a_loop_a_previous_value_node_is_already_in_is_linked_as_asked() {
    let (document, add, previous_value) = add_linked_into_scale(PREVIOUS_VALUE_NODE_TYPE);
    assert!(
        !the_link_closes_a_loop_of_function_nodes_no_previous_value_node_is_in(
            &document,
            &a_link_between(previous_value, add),
            &|| Ok(HashSet::from([add, previous_value])),
        )
        .unwrap()
    );
}

#[test]
fn a_link_closing_no_loop_reads_nothing_of_the_running_stream() {
    let (document, add, scale) = add_linked_into_scale("maths:Scale");
    let another_link_the_same_way = StreamGraphDocumentLink {
        target: StreamGraphDocumentLinkEnd {
            node: scale,
            port: "y".to_owned(),
        },
        ..a_link_between(add, scale)
    };
    assert!(
        !the_link_closes_a_loop_of_function_nodes_no_previous_value_node_is_in(
            &document,
            &another_link_the_same_way,
            &|| Err("the running stream was read".into()),
        )
        .unwrap()
    );
}

#[test]
fn a_project_file_is_imported_as_its_dotted_path_and_a_package_as_its_folder() {
    let project_directory = Path::new("/rig");
    for (changed_file, module) in [
        ("generators/looks.py", Some("generators.looks")),
        ("/rig/generators/looks.py", Some("generators.looks")),
        ("generators/__init__.py", Some("generators")),
        ("stream.py", Some("stream")),
        ("streams/main.graph.json", None),
        ("/elsewhere/looks.py", None),
    ] {
        assert_eq!(
            the_module_a_project_file_is_imported_as(project_directory, Path::new(changed_file))
                .as_deref(),
            module,
            "{changed_file}"
        );
    }
}

#[test]
fn an_edit_the_documents_hold_that_a_running_stream_refuses_is_held_with_each_later_one_of_it() {
    use crate::control_plane_stub_support::STUB_STREAM_ID;
    use crate::mcp::tests::ControlPlaneMcpDispatchStubRuntime;

    let refusal = "the function node pool is already running";
    let runtime =
        ControlPlaneMcpDispatchStubRuntime::refusing_each_edit_the_documents_hold_first(refusal);
    let tokio_runtime = tokio::runtime::Runtime::new().unwrap();
    let project_repositories =
        streamlib_project_repository::ProjectRepositoriesOpenInThisRuntime::default();
    let executor = super::StreamGraphTransactionExecutor {
        operations_on_the_loaded_streams: &runtime,
        project_repositories: &project_repositories,
        async_runtime: tokio_runtime.handle(),
    };
    let carried = StreamGraphOperation::CarryTheLinksOntoThePortsOfAPlacedStream {
        placed_stream: Uuid::new_v4(),
        port_links_taken_out: Vec::new(),
        port_links_put_back: Vec::new(),
    };
    let renamed = StreamGraphOperation::RenameNode {
        node: Uuid::new_v4(),
        from: "scale".to_owned(),
        to: "zoom".to_owned(),
    };

    executor.make_live_what_the_documents_now_hold(vec![
        (STUB_STREAM_ID.to_owned(), carried.clone()),
        (STUB_STREAM_ID.to_owned(), renamed.clone()),
        ("a-stream-not-loaded".to_owned(), renamed.clone()),
    ]);

    assert_eq!(
        *runtime.recorded_graph_mutations.lock(),
        std::slice::from_ref(&carried),
        "the later edit of the stream that refused is not sent to it"
    );
    let held = runtime.recorded_edits_held_not_live.lock().clone();
    assert_eq!(held.len(), 2, "{held:?}");
    assert_eq!(held[0].0, carried);
    assert!(held[0].1.contains(refusal), "{held:?}");
    assert_eq!(held[1].0, renamed);
    assert!(
        held[1]
            .1
            .contains("an earlier edit of the same transaction was not taken")
            && held[1].1.contains(refusal),
        "{held:?}"
    );
}
