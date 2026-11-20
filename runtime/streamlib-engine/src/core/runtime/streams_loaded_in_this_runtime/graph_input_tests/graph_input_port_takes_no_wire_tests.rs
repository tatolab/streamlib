// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A setting of a `GraphInput` read at setup takes no wire, as every setting of one does.

use super::*;

/// A Value wire into a setting of a `GraphInput` read at setup, its `port`, is refused: the
/// setting declares the stream's interface rather than running on a value.
///
/// Mental-revert: without the `GraphInput` rule a String Value wire into `port` connects.
#[test]
#[serial]
fn a_value_wire_into_the_port_of_a_graph_input_is_refused() {
    let project_directory = a_project_on_disk();
    let name = Uuid::new_v4();
    let echo = Uuid::new_v4();
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    load_the_graph_inputs(
        &stream,
        vec![
            a_graph_input_in_a_document(
                name,
                "name",
                json!({ "port": "name", "pin_type": { "type": "string" } }),
            ),
            a_graph_input_in_a_document(
                echo,
                "echo",
                json!({ "port": "echo", "family": "records" }),
            ),
        ],
    );

    let refusal = stream
        .connect(
            OutputLinkPortRef::new(the_run_id_of(&stream, name), VALUE),
            InputLinkPortRef::new(the_run_id_of(&stream, echo), "port"),
        )
        .expect_err("a wire into a setting of a GraphInput is refused")
        .to_string();

    assert!(
        refusal.contains("input `port` of `echo`, a setting, which takes no wire"),
        "{refusal}"
    );
    assert_eq!(stream.to_json().unwrap()["links"], json!([]));
}

/// A stream document holding a link into the `port` of a top-level `GraphInput`, a setting read
/// at setup, is refused as it is validated, naming the setting, before the load adds any node.
///
/// Mental-revert: judging the document's link by its mode alone, without the `GraphInput` rule,
/// loads the link.
#[test]
#[serial]
fn a_document_linking_into_the_port_of_a_graph_input_is_refused_at_load() {
    let project_directory = a_project_on_disk();
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    let document = json!({
        "nodes": [
            { "name": "cues", "type": MockOutputOnlyNode::node_class_import_path().as_str() },
            a_graph_input_in_a_document(
                Uuid::new_v4(), "echo", json!({ "port": "echo", "family": "records" }),
            ),
        ],
        "links": [{
            "source": { "node": "cues", "port": "out1" },
            "target": { "node": "echo", "port": "port" },
        }],
    });

    let refusal = stream
        .load_graph_snapshot_into_this_stream(
            &GraphSnapshot::from_graph_document(document).unwrap(),
            None,
        )
        .expect_err("a document linking into a setting of a GraphInput is refused")
        .to_string();

    assert!(
        refusal.contains("input `port` of `echo`, a setting, which takes no wire"),
        "{refusal}"
    );
    assert_eq!(stream.node_count(), 0, "the refused load added no node");
}
