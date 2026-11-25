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
        None,
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
            .map(|rendered| rendered["input_values"].clone())
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
                    member: None,
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
                    member: None,
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

/// A value set live on a function node's input, and on a shader node's, is held apart from the
/// graph, so `graph` renders it live and the saved pins are unchanged: the shader's evaluator
/// takes it as it runs.
///
/// Mental-revert: holding a shader node's value on the graph answers `OnTheGraph`, and a running
/// shader, built once from its pins, never takes it.
#[test]
#[serial_test::serial]
fn a_live_set_on_a_function_node_and_on_a_shader_node_is_held_apart_from_the_graph() {
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
                member: None,
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
    assert!(matches!(
        set_live(tint, "amount", json!(0.25)),
        InputValuesSetLive::ApartFromTheGraph(_)
    ));

    let graph = stream.to_json().expect("the graph serializes");
    for (node, input, value) in [(scale, "x", json!(0.5)), (tint, "amount", json!(0.25))] {
        let rendered = graph["nodes"]
            .as_array()
            .and_then(|nodes| {
                nodes
                    .iter()
                    .find(|rendered| rendered["id"] == node.to_string())
            })
            .unwrap_or_else(|| panic!("the graph holds {node}: {graph}"));
        let rendered_input = rendered["ports"]["inputs"]
            .as_array()
            .and_then(|inputs| inputs.iter().find(|port| port["name"] == input))
            .unwrap_or_else(|| panic!("{node} renders `{input}`: {rendered}"));
        assert_eq!(
            (
                &rendered_input["value"],
                &rendered_input["value_comes_from"]
            ),
            (&value, &json!("live"))
        );
        assert!(rendered["pins"].get(input).is_none(), "{rendered}");
    }
}

/// A saved value set on a shader node's input, as one on a function node's input, announces only
/// its input value change and no graph change, so no commit begins: the shader's evaluator is
/// handed the value as it runs.
///
/// Mental-revert: announcing a graph change for a shader input begins a commit that rebuilds
/// nothing, and the running shader keeps the value it was built with.
#[test]
#[serial_test::serial]
fn a_saved_set_on_a_shader_or_function_node_input_announces_no_graph_change() {
    use std::sync::Arc;

    use parking_lot::Mutex;

    use crate::core::nodes::NodeSpec;
    use crate::core::pubsub::{Event, EventListener, PUBSUB, RuntimeEvent, topics};
    use crate::core::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
    use crate::core::runtime::{Runner, RuntimeOperations};
    use streamlib_project_repository::StreamGraphOperation;

    #[derive(Default)]
    struct ChangesCounted {
        graph_changes: u64,
        input_value_changes: u64,
    }
    impl EventListener for ChangesCounted {
        fn on_event(&mut self, event: &Event) -> crate::core::error::Result<()> {
            if let Event::OfALoadedStream { event, .. } = event {
                match event {
                    RuntimeEvent::GraphDidChange => self.graph_changes += 1,
                    RuntimeEvent::InputValueDidChange { .. } => self.input_value_changes += 1,
                    _ => {}
                }
            }
            Ok(())
        }

        fn on_events_lost(&mut self, lost_event_count: u64) -> crate::core::error::Result<()> {
            panic!("the stream's topic lost {lost_event_count} events, which this count needs");
        }
    }

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
    let counted = Arc::new(Mutex::new(ChangesCounted::default()));
    PUBSUB
        .subscribe(
            &topics::loaded_stream(stream.loaded_stream_identity()),
            Arc::clone(&counted) as Arc<Mutex<dyn EventListener>>,
        )
        .expect("the stream's topic is subscribed");
    let changes_once_delivered = |input_value_changes: u64| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while counted.lock().input_value_changes < input_value_changes
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
        let counted = counted.lock();
        (counted.graph_changes, counted.input_value_changes)
    };
    let save = |node, input: &str, value| {
        tokio_runtime
            .block_on(stream.apply_a_stream_graph_operation_live(
                StreamGraphOperation::SetNodeInputValue {
                    member: None,
                    node,
                    input: input.to_owned(),
                    from: None,
                    to: Some(value),
                },
            ))
            .unwrap_or_else(|refusal| panic!("`{input}` is saved: {refusal}"));
    };

    save(scale, "x", json!(0.5));
    assert_eq!(
        changes_once_delivered(1),
        (0, 1),
        "a saved set read each run announces its input and no graph change"
    );
    save(tint, "amount", json!(0.25));
    assert_eq!(
        changes_once_delivered(2),
        (0, 2),
        "a saved set on a shader input announces its input and no graph change"
    );
}
