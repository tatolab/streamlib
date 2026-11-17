// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The input values of a function node and a shader node, each of a type its stream's node
//! interpreter describes, are checked as a process node's are: a typed input takes what its pin
//! type takes within its declared range, an input that takes only a link refuses a value, and a
//! name that is none of its inputs is not found; a value set live reaches each node.

use serde_json::json;

use super::*;
use crate::core::compiler::compiler_ops::function_node_island::tests::a_function_node_type;
use crate::core::compiler::compiler_ops::shader_node_island_program::tests::shader_program;
use crate::core::descriptors::PortFamily;
use crate::core::descriptors::{
    NodeClassShortName, NodeDescriptor, PinDeclaration, PinType, PortDescriptor,
    PortFamilyDeclaration,
};
use crate::core::error::Error;
use crate::core::nodes::HowANodeOfADescribedTypeRuns;

const SCALE: &str = "test_ops:scale";

const TINT: &str = "test_shaders:tint";

/// The node types `scale`, a function node type whose `x` is a Float and whose `y` names no pin
/// type, and `tint`, a shader node type taking an Image `image` and a Float `amount` from 0 to 1.
fn node_types_resolving_a_function_and_a_shader_node_type() -> NodeTypesOneStreamResolves {
    let node_types = NodeTypesOneStreamResolves::new();
    register_a_function_and_a_shader_node_type(&node_types);
    node_types
}

/// Register `scale` and `tint` in `node_types`.
fn register_a_function_and_a_shader_node_type(node_types: &NodeTypesOneStreamResolves) {
    let mut scale = a_function_node_type(SCALE, &["y"], &["result"]);
    scale = scale.with_input(
        PortDescriptor::new(
            "x",
            "",
            true,
            PortFamilyDeclaration::of_family(PortFamily::Channels),
        )
        .with_delivery_profile("newest")
        .with_pin_declaration(PinDeclaration::exactly(PinType::Float { exact_size: None })),
    );
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            scale,
            HowANodeOfADescribedTypeRuns::InsideTheReadOfEachNodeItFeeds {
                node_once_promoted_to_its_own_process: Box::new(|node| {
                    Err(Error::Configuration(format!(
                        "`{}` builds no node in this test",
                        node.id
                    )))
                }),
            },
        )
        .unwrap();

    let mut tint = NodeDescriptor::new(
        NodeClassShortName::new("tint").unwrap(),
        NodeClassImportPath::new(TINT).unwrap(),
        "a shader node type for a test",
    );
    tint.node_kind = NodeKind::Shader;
    let mut amount = PinDeclaration::exactly(PinType::Float { exact_size: None });
    amount.annotations.range = Some(PinNumberRange {
        minimum: Some(0.0),
        maximum: Some(1.0),
    });
    tint = tint
        .with_input(
            PortDescriptor::new(
                "image",
                "",
                true,
                PortFamilyDeclaration::of_family(PortFamily::Image),
            )
            .with_delivery_profile("newest"),
        )
        .with_input(
            PortDescriptor::new(
                "amount",
                "",
                true,
                PortFamilyDeclaration::of_family(PortFamily::Channels),
            )
            .with_delivery_profile("newest")
            .with_pin_declaration(amount),
        )
        .with_output(PortDescriptor::new(
            "result",
            "",
            true,
            PortFamilyDeclaration::of_family(PortFamily::Image),
        ));
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            tint,
            HowANodeOfADescribedTypeRuns::OnTheNativeShaderEvaluatorOfItsIsland {
                shader_node_program: shader_program(
                    "tint",
                    "return image.pixel * amount;",
                    &["image"],
                    &["amount"],
                ),
                evaluator_of_the_island_it_hosts: Box::new(|node| {
                    Err(Error::Configuration(format!(
                        "`{}` builds no node in this test",
                        node.id
                    )))
                }),
            },
        )
        .unwrap();
}

/// The refusal, if any, of `value` set on input `input` of a node of `node_type` named `node`.
fn the_refusal_of(
    node_types: &NodeTypesOneStreamResolves,
    node_type: &str,
    input: &str,
    value: Option<Value>,
) -> Result<(), InputValueRefusal> {
    let mut input_values_after_the_set = Map::new();
    if let Some(value) = &value {
        input_values_after_the_set.insert(input.to_owned(), value.clone());
    }
    refuse_an_input_value_its_input_does_not_take(
        node_types,
        &NodeAnInputValueIsSetOn {
            name: "node",
            node_type: &NodeClassImportPath::new(node_type).unwrap(),
            input_values_after_the_set: &input_values_after_the_set,
        },
        input,
        value.as_ref(),
    )
}

fn the_code_of(refusal: Result<(), InputValueRefusal>) -> InputValueRefusalCode {
    refusal.expect_err("the value is refused").code
}

#[test]
fn a_function_nodes_input_values_are_checked() {
    let node_types = node_types_resolving_a_function_and_a_shader_node_type();
    assert_eq!(
        the_refusal_of(&node_types, SCALE, "x", Some(json!(0.5))),
        Ok(())
    );
    assert_eq!(the_refusal_of(&node_types, SCALE, "x", None), Ok(()));
    assert_eq!(
        the_code_of(the_refusal_of(&node_types, SCALE, "x", Some(json!("loud")))),
        InputValueRefusalCode::InputValueNotValid
    );
    assert_eq!(
        the_refusal_of(&node_types, SCALE, "y", Some(json!("any value"))),
        Ok(()),
        "an input naming no pin type takes any value a function reads"
    );
    assert_eq!(
        the_code_of(the_refusal_of(&node_types, SCALE, "z", Some(json!(1)))),
        InputValueRefusalCode::PortNotFound
    );
    assert!(!node_type_takes_a_config(
        &node_types,
        &NodeClassImportPath::new(SCALE).unwrap()
    ));
}

#[test]
fn a_shader_nodes_input_values_are_checked() {
    let node_types = node_types_resolving_a_function_and_a_shader_node_type();
    assert_eq!(
        the_refusal_of(&node_types, TINT, "amount", Some(json!(0.5))),
        Ok(())
    );
    assert_eq!(
        the_code_of(the_refusal_of(
            &node_types,
            TINT,
            "amount",
            Some(json!(2.0))
        )),
        InputValueRefusalCode::InputValueNotValid,
        "2 lies outside the range and is never clamped"
    );
    assert_eq!(
        the_code_of(the_refusal_of(
            &node_types,
            TINT,
            "amount",
            Some(json!(true))
        )),
        InputValueRefusalCode::InputValueNotValid
    );
    assert_eq!(
        the_code_of(the_refusal_of(&node_types, TINT, "image", Some(json!(1)))),
        InputValueRefusalCode::InputTakesNoValue
    );
    assert_eq!(
        the_code_of(the_refusal_of(
            &node_types,
            TINT,
            "brightness",
            Some(json!(1))
        )),
        InputValueRefusalCode::PortNotFound
    );
}

/// A value set live on a function node and on a shader node of a loaded stream not started is
/// the pin value each node holds in the graph; taken off, the node holds none. It proves what the
/// graph holds, not that a running shader's evaluator reads it.
#[test]
#[serial_test::serial]
fn a_value_set_live_on_a_function_node_and_a_shader_node_is_the_pin_value_each_holds() {
    use crate::core::nodes::NodeSpec;
    use crate::core::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
    use crate::core::runtime::{Runner, RuntimeOperations};
    use streamlib_project_repository::StreamGraphOperation;

    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");
    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory.path(),
            "main",
        ))
        .expect("an empty stream loads");
    register_a_function_and_a_shader_node_type(stream.node_types_this_stream_resolves());
    let tokio_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime");
    let the_pins_of = |node: uuid::Uuid| {
        let graph = stream.to_json().expect("the graph serializes");
        graph["nodes"]
            .as_array()
            .and_then(|nodes| {
                nodes
                    .iter()
                    .find(|rendered| rendered["id"] == node.to_string())
            })
            .map(|rendered| rendered["pins"].clone())
            .unwrap_or_else(|| panic!("the graph holds node {node}: {graph}"))
    };

    for (node_type, input, value) in [(SCALE, "x", json!(0.5)), (TINT, "amount", json!(0.25))] {
        let node = uuid::Uuid::new_v4();
        stream
            .add_node(
                NodeSpec::new(NodeClassImportPath::new(node_type).unwrap(), Value::Null)
                    .with_node_uuid(node),
            )
            .unwrap_or_else(|refusal| panic!("a `{node_type}` node is added: {refusal}"));
        tokio_runtime
            .block_on(stream.apply_a_stream_graph_operation_live(
                StreamGraphOperation::SetNodeInputValue {
                    node,
                    input: input.to_owned(),
                    from: None,
                    to: Some(value.clone()),
                },
            ))
            .unwrap_or_else(|refusal| panic!("`{input}` of `{node_type}` is set: {refusal}"));
        assert_eq!(the_pins_of(node)[input], value, "{node_type}");
        tokio_runtime
            .block_on(stream.apply_a_stream_graph_operation_live(
                StreamGraphOperation::SetNodeInputValue {
                    node,
                    input: input.to_owned(),
                    from: Some(value),
                    to: None,
                },
            ))
            .unwrap_or_else(|refusal| panic!("`{input}` of `{node_type}` is taken off: {refusal}"));
        assert!(
            the_pins_of(node).get(input).is_none(),
            "{node_type}: {}",
            the_pins_of(node)
        );
    }
}

/// A value set live on a function node's input is held apart from the graph, so `graph` renders
/// it live and its saved pins are unchanged; one set on a shader node's input is held on the
/// graph, as its evaluator is built from the values the graph holds.
///
/// Mental-revert: holding a shader node's value live renders it `live` while the shader's
/// evaluator, built from its pins, reads another.
#[test]
#[serial_test::serial]
fn a_live_set_on_a_function_node_is_held_apart_and_one_on_a_shader_node_on_the_graph() {
    use crate::core::graph::input_values::{InputValueToSetLive, InputValuesSetLive};
    use crate::core::nodes::NodeSpec;
    use crate::core::runtime::Runner;
    use crate::core::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;

    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");
    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory.path(),
            "main",
        ))
        .expect("an empty stream loads");
    register_a_function_and_a_shader_node_type(stream.node_types_this_stream_resolves());
    let added = |node_type: &str| {
        let node = uuid::Uuid::new_v4();
        stream
            .add_node(
                NodeSpec::new(NodeClassImportPath::new(node_type).unwrap(), Value::Null)
                    .with_node_uuid(node),
            )
            .unwrap_or_else(|refusal| panic!("a `{node_type}` node is added: {refusal}"));
        node
    };
    let scale = added(SCALE);
    let tint = added(TINT);
    let set_live = |node, input: &str, value| {
        stream
            .set_input_values_live(&[InputValueToSetLive {
                node,
                input: input.to_owned(),
                value: Some(value),
            }])
            .unwrap_or_else(|refusal| panic!("`{input}` is set: {refusal:?}"))
    };

    assert!(matches!(
        set_live(scale, "x", json!(0.5)),
        InputValuesSetLive::ApartFromTheGraph(_)
    ));
    assert_eq!(
        set_live(tint, "amount", json!(0.25)),
        InputValuesSetLive::OnTheGraph
    );

    let graph = stream.to_json().expect("the graph serializes");
    let rendered_scale = graph["nodes"]
        .as_array()
        .and_then(|nodes| nodes.iter().find(|node| node["id"] == scale.to_string()))
        .unwrap_or_else(|| panic!("the graph holds `scale`: {graph}"));
    let x = rendered_scale["ports"]["inputs"]
        .as_array()
        .and_then(|inputs| inputs.iter().find(|port| port["name"] == "x"))
        .unwrap_or_else(|| panic!("`scale` renders `x`: {rendered_scale}"));
    assert_eq!(
        (&x["value"], &x["value_comes_from"]),
        (&json!(0.5), &json!("live"))
    );
    assert!(
        rendered_scale["pins"].get("x").is_none(),
        "{rendered_scale}"
    );
}
