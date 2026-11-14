// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Each node of a `tatolab-ops` type in a project's stream documents, moved to the Core Nodes
//! type that replaced it: the same function under the same name, ports and config.

use uuid::Uuid;

use crate::core_project_import::CORE_PROJECT_IMPORT_FOLDER;
use crate::project_asset::PROJECT_IMPORTS_DIRECTORY;
use crate::project_transaction::{ProjectOperation, StreamGraphOperationOnAStream};
use crate::stream_graph_document::{
    PREVIOUS_VALUE_NODE_TYPE, StreamGraphDocument, StreamGraphDocumentNode, StreamGraphOperation,
};

/// The key a project's history records the move under, so it is made once and an undo holds.
pub const TATOLAB_OPS_NODES_MOVED_TO_CORE_NODES_KEY: &str =
    "tatolab-runtime-moves-tatolab-ops-nodes-to-core-nodes";

/// The module `tatolab-ops` held every node type in but `ImageFile`.
const TATOLAB_OPS_MODULE: &str = "tatolab.ops";

/// The `tatolab-ops` type a loop of function nodes closed through, which `PreviousValue` replaced.
const TATOLAB_OPS_FEEDBACK_NODE: &str = "Feedback";

/// `tatolab-ops`' `ImageFile`, the one node type it held outside its top module.
const TATOLAB_OPS_IMAGE_FILE_TYPE: &str = "tatolab.ops.image_file:ImageFile";

/// The Core Nodes module, under its package `core_nodes`, that holds each `tatolab-ops` node by
/// the name both give it.
const CORE_NODES_MODULE_OF_EACH_TATOLAB_OPS_NODE: &[(&str, &str)] = &[
    ("Lfo", "animation"),
    ("Timer", "animation"),
    ("Lag", "animation"),
    ("Remap", "math_nodes"),
    ("Clamp", "math_nodes"),
    ("Add", "math_nodes"),
    ("Subtract", "math_nodes"),
    ("Multiply", "math_nodes"),
    ("Divide", "math_nodes"),
    ("Power", "math_nodes"),
    ("Minimum", "math_nodes"),
    ("Maximum", "math_nodes"),
    ("Absolute", "math_nodes"),
    ("Modulo", "math_nodes"),
    ("Lerp", "math_nodes"),
    ("Smoothstep", "math_nodes"),
    ("Step", "math_nodes"),
    ("Constant", "math_nodes"),
    ("Random", "math_nodes"),
    ("PerlinNoise", "math_nodes"),
    ("Compare", "logic"),
    ("Gate", "logic"),
    ("Switch", "logic"),
    ("Select", "logic"),
    ("Trigger", "logic"),
    ("Counter", "logic"),
    ("SampleAndHold", "logic"),
    ("HoldFrame", "logic"),
];

/// A Core Nodes node type as a project that imports Core Nodes names it.
fn core_nodes_type(module: &str, node: &str) -> String {
    format!("{PROJECT_IMPORTS_DIRECTORY}/{CORE_PROJECT_IMPORT_FOLDER}/core_nodes.{module}:{node}")
}

/// The type that replaced the `tatolab-ops` type `node_type`, `None` for any other type.
pub fn the_type_that_replaced_a_tatolab_ops_type(node_type: &str) -> Option<String> {
    if node_type == TATOLAB_OPS_IMAGE_FILE_TYPE {
        return Some(core_nodes_type("image_file", "ImageFile"));
    }
    let node = node_type
        .strip_prefix(TATOLAB_OPS_MODULE)?
        .strip_prefix(':')?;
    if node == TATOLAB_OPS_FEEDBACK_NODE {
        return Some(PREVIOUS_VALUE_NODE_TYPE.to_owned());
    }
    CORE_NODES_MODULE_OF_EACH_TATOLAB_OPS_NODE
        .iter()
        .find(|(tatolab_ops_node, _)| *tatolab_ops_node == node)
        .map(|(_, module)| core_nodes_type(module, node))
}

/// The operations that move each node of a `tatolab-ops` type on the level of the stream
/// asset `stream` that `document` holds to the type that replaced it: the node taken out with
/// its links and placed again under its UUID, name, config, pins and studio fields with its new
/// type and the same links, so an undo puts back exactly what was there.
pub fn operations_moving_each_tatolab_ops_node_to_core_nodes(
    stream: Uuid,
    document: &StreamGraphDocument,
) -> Vec<ProjectOperation> {
    let on_the_stream = |graph_operation| {
        ProjectOperation::StreamGraph(StreamGraphOperationOnAStream {
            stream,
            graph_operation,
        })
    };
    document
        .nodes()
        .into_iter()
        .filter_map(|node| {
            let replacing_type = the_type_that_replaced_a_tatolab_ops_type(&node.node_type)?;
            let links = document.links_touching(node.id);
            let moved = StreamGraphDocumentNode {
                node_type: replacing_type,
                ..node.clone()
            };
            Some([
                on_the_stream(StreamGraphOperation::RemoveNode {
                    node,
                    links: links.clone(),
                }),
                on_the_stream(StreamGraphOperation::AddNode { node: moved, links }),
            ])
        })
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn each_tatolab_ops_type_is_replaced_by_its_core_nodes_type() {
        assert_eq!(
            the_type_that_replaced_a_tatolab_ops_type("tatolab.ops:Lfo").as_deref(),
            Some("imports/core-nodes/core_nodes.animation:Lfo")
        );
        assert_eq!(
            the_type_that_replaced_a_tatolab_ops_type("tatolab.ops:Switch").as_deref(),
            Some("imports/core-nodes/core_nodes.logic:Switch")
        );
        assert_eq!(
            the_type_that_replaced_a_tatolab_ops_type("tatolab.ops:PerlinNoise").as_deref(),
            Some("imports/core-nodes/core_nodes.math_nodes:PerlinNoise")
        );
        assert_eq!(
            the_type_that_replaced_a_tatolab_ops_type("tatolab.ops.image_file:ImageFile")
                .as_deref(),
            Some("imports/core-nodes/core_nodes.image_file:ImageFile")
        );
        assert_eq!(
            the_type_that_replaced_a_tatolab_ops_type("tatolab.ops:Feedback").as_deref(),
            Some("tatolab.stream:PreviousValue")
        );
    }

    #[test]
    fn a_type_tatolab_ops_never_held_is_left_alone() {
        for node_type in [
            "tatolab.ops:NoSuchNode",
            "tatolab.opsy:Lfo",
            "tatolab.ops.other:Lfo",
            "imports/core-nodes/core_nodes.animation:Lfo",
            "tatolab.stream:PreviousValue",
            "nodes.tone:Tone",
        ] {
            assert_eq!(the_type_that_replaced_a_tatolab_ops_type(node_type), None);
        }
    }

    fn a_document_with_an_lfo_driving_a_tone() -> StreamGraphDocument {
        StreamGraphDocument::from_value(json!({
            "nodes": [
                {
                    "id": "00000000-0000-0000-0000-000000000001",
                    "name": "lfo",
                    "type": "tatolab.ops:Lfo",
                    "config": {},
                    "pins": {"rate_hz": 0.5},
                    "studio": {"position": {"x": 10.0, "y": 20.0}}
                },
                {
                    "id": "00000000-0000-0000-0000-000000000002",
                    "name": "tone",
                    "type": "nodes.tone:Tone",
                    "config": {}
                }
            ],
            "links": [
                {
                    "source": {"node": "00000000-0000-0000-0000-000000000001", "port": "out"},
                    "target": {"node": "00000000-0000-0000-0000-000000000002", "port": "pitch"}
                }
            ]
        }))
        .expect("the document")
    }

    #[test]
    fn a_tatolab_ops_node_is_placed_again_under_its_core_nodes_type_with_everything_else_kept() {
        let mut document = a_document_with_an_lfo_driving_a_tone();
        let lfo_before = document.node_named("lfo").expect("the lfo before the move");
        let links_before = document.links();
        let stream = Uuid::from_u128(7);

        let operations = operations_moving_each_tatolab_ops_node_to_core_nodes(stream, &document);
        assert_eq!(operations.len(), 2);
        for operation in &operations {
            let ProjectOperation::StreamGraph(on_the_stream) = operation else {
                panic!("a move edits a stream's graph, not {operation:?}");
            };
            assert_eq!(on_the_stream.stream, stream);
            document
                .apply(&on_the_stream.graph_operation)
                .expect("the move applies");
        }

        let lfo_after = document.node_named("lfo").expect("the lfo after the move");
        assert_eq!(
            lfo_after,
            StreamGraphDocumentNode {
                node_type: "imports/core-nodes/core_nodes.animation:Lfo".to_owned(),
                ..lfo_before
            }
        );
        assert_eq!(document.links(), links_before);
        assert_eq!(
            document.node_named("tone").expect("the tone").node_type,
            "nodes.tone:Tone"
        );
    }

    #[test]
    fn the_inverse_of_a_move_puts_the_tatolab_ops_node_back() {
        let mut document = a_document_with_an_lfo_driving_a_tone();
        let lfo_before = document.node_named("lfo").expect("the lfo before the move");
        let links_before = document.links();
        let operations =
            operations_moving_each_tatolab_ops_node_to_core_nodes(Uuid::from_u128(7), &document);
        let graph_operations: Vec<StreamGraphOperation> = operations
            .iter()
            .filter_map(|operation| match operation {
                ProjectOperation::StreamGraph(on_the_stream) => {
                    Some(on_the_stream.graph_operation.clone())
                }
                _ => None,
            })
            .collect();
        for graph_operation in &graph_operations {
            document.apply(graph_operation).expect("the move applies");
        }
        for graph_operation in graph_operations.iter().rev() {
            document
                .apply(&graph_operation.inverse())
                .expect("its inverse applies");
        }
        assert_eq!(document.node_named("lfo"), Some(lfo_before));
        assert_eq!(document.links(), links_before);
    }

    #[test]
    fn a_document_holding_no_tatolab_ops_node_needs_no_operation() {
        let document = StreamGraphDocument::from_value(json!({
            "nodes": [{
                "id": "00000000-0000-0000-0000-000000000002",
                "name": "tone",
                "type": "nodes.tone:Tone",
                "config": {}
            }],
            "links": []
        }))
        .expect("the document");
        assert!(
            operations_moving_each_tatolab_ops_node_to_core_nodes(Uuid::from_u128(7), &document)
                .is_empty()
        );
    }
}
