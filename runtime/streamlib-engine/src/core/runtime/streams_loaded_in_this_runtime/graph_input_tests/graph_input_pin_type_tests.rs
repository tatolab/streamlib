// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A `GraphInput` whose `pin_type` setting chooses its type: the value it publishes, how it
//! renders, a change its link refuses and a change that rebuilds it.

use super::*;
use crate::core::pin_value_packing::{PackedPinValueRef, unpack_pin_value};
use crate::core::test_support::MockPinTypedNode;

/// The refusal of a saved set of `input` on `node` to `value`, in words.
fn the_refusal_of_a_saved_input(
    stream: &LoadedStreamInThisRuntime,
    async_runtime: &tokio::runtime::Runtime,
    node: Uuid,
    input: &str,
    value: Value,
) -> String {
    match async_runtime.block_on(stream.apply_a_stream_graph_operation_live(
        StreamGraphOperation::SetNodeInputValue {
            member: None,
            node,
            input: input.to_owned(),
            from: None,
            to: Some(value),
        },
    )) {
        Ok(_) => panic!("the set of `{input}` is refused"),
        Err(refusal) => refusal.to_string(),
    }
}

/// The value of `pin_type` the next message `reader` reads carries.
fn the_typed_value_read(reader: &mut OutputPortViewerReader, pin_type: &Value) -> Value {
    let message = the_next_message_read(reader);
    unpack_pin_value(
        &serde_json::from_value(pin_type.clone()).unwrap(),
        PackedPinValueRef {
            payload_descriptor: &message.payload_descriptor,
            payload: &message.payload,
        },
    )
    .expect("the message reads as a value of the chosen type")
}

/// A `GraphInput` choosing Float publishes its value as a Float, renders the type on its
/// `value` output and on the stream's interface, and renders its `pin_type` setting reading the
/// saved choice; one linked into a Float input refuses a change to String.
///
/// Mental-revert: without the chosen type typing the port, the value travels as a record and
/// does not unpack as a Float; without judging the wired links, the change to String is taken.
#[test]
#[serial]
fn a_graph_input_choosing_a_pin_type_publishes_and_renders_it_and_refuses_one_its_link_does_not_take()
 {
    let project_directory = a_project_on_disk();
    let async_runtime = an_async_runtime();
    let level = Uuid::new_v4();
    let gain = Uuid::new_v4();
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    let float = json!({ "type": "float" });
    let document = json!({
        "nodes": [
            a_graph_input_in_a_document(
                level, "level", json!({ "port": "level", "pin_type": float, "value": 0.5 }),
            ),
            a_graph_input_in_a_document(gain, "gain", json!({ "port": "gain", "pin_type": float })),
            { "name": "meter", "type": MockPinTypedNode::node_class_import_path().as_str() },
        ],
        "links": [{
            "source": { "node": "gain", "port": VALUE },
            "target": { "node": "meter", "port": "gain" },
        }],
    });
    stream
        .load_graph_snapshot_into_this_stream(
            &GraphSnapshot::from_graph_document(document).unwrap(),
            None,
        )
        .expect("the graph loads");
    play(&stream);
    let (_viewer, mut reader) = a_reader_of_the_value_of(&stream, &async_runtime, level);

    assert_eq!(the_typed_value_read(&mut reader, &float), json!(0.5));
    let rendered = the_rendered_node(&stream, level);
    assert_eq!(
        rendered["components"]["graph_interface"]["pin_type"], float,
        "{rendered}"
    );
    let value_output = rendered["ports"]["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|output| output["name"] == VALUE)
        .unwrap_or_else(|| panic!("`value` renders: {rendered}"));
    assert_eq!(value_output["pin_type"], float, "{rendered}");
    let pin_type_input = rendered["ports"]["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|input| input["name"] == "pin_type")
        .unwrap_or_else(|| panic!("`pin_type` renders: {rendered}"));
    assert_eq!(pin_type_input["value"], float, "{rendered}");
    assert_eq!(pin_type_input["value_comes_from"], "saved", "{rendered}");

    let refusal = the_refusal_of_a_saved_input(
        &stream,
        &async_runtime,
        gain,
        "pin_type",
        json!({ "type": "string" }),
    );
    assert!(refusal.contains("`meter.gain` (Float)"), "{refusal}");
    assert_eq!(
        the_rendered_node(&stream, gain)["components"]["graph_interface"]["pin_type"],
        float
    );
    stream.stop().expect("the stream ends");
}

/// A change of the type a running `GraphInput` chooses leaves it to rebuild, and once rebuilt
/// its reader reads values of the new type.
///
/// Mental-revert: without leaving the node to rebuild when the reconcile changes a setting read
/// at setup, nothing is rebuilt and the node still publishes a Float.
#[test]
#[serial]
fn a_change_of_the_type_a_running_graph_input_chooses_rebuilds_it() {
    let project_directory = a_project_on_disk();
    let async_runtime = an_async_runtime();
    let level = Uuid::new_v4();
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    load_the_graph_inputs(
        &stream,
        vec![a_graph_input_in_a_document(
            level,
            "level",
            json!({ "port": "level", "pin_type": { "type": "float" }, "value": 0.5 }),
        )],
    );
    play(&stream);

    set_the_saved_input(&stream, &async_runtime, level, "value", json!(3));
    let integer = json!({ "type": "integer" });
    set_the_saved_input(&stream, &async_runtime, level, "pin_type", integer.clone());
    let rebuilt = async_runtime
        .block_on(RuntimeOperations::rebuild_each_node_a_setting_read_at_setup_changed(&*stream))
        .expect("the GraphInput rebuilds with the new type");

    assert_eq!(rebuilt, vec![level]);
    let (_viewer, mut reader) = a_reader_of_the_value_of(&stream, &async_runtime, level);
    assert_eq!(the_typed_value_read(&mut reader, &integer), json!(3));
    stream.stop().expect("the stream ends");
}

/// A `GraphInput` choosing a Float, saved with a `control` and a `range`, renders them as the
/// annotations of its `value` input in `graph`; a saved set of an annotation its type does not
/// take is refused naming it, and the Input keeps the annotations it had.
///
/// Mental-revert: without `graph` rendering a port's annotations, the `value` input renders
/// none and Perform cannot draw the dial.
#[test]
#[serial]
fn a_graph_input_renders_the_annotations_it_saves_on_its_value_input_and_refuses_one_its_type_does_not_take()
 {
    let project_directory = a_project_on_disk();
    let async_runtime = an_async_runtime();
    let lines = Uuid::new_v4();
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    load_the_graph_inputs(
        &stream,
        vec![a_graph_input_in_a_document(
            lines,
            "lines",
            json!({
                "port": "lines", "pin_type": { "type": "float" }, "value": 2,
                "range": [1, 8], "control": "dial",
            }),
        )],
    );
    let annotations = json!({ "range": { "minimum": 1.0, "maximum": 8.0 }, "control": "dial" });
    let the_value_input_rendered = || {
        let rendered = the_rendered_node(&stream, lines);
        rendered["ports"]["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|input| input["name"] == VALUE)
            .cloned()
            .unwrap_or_else(|| panic!("`value` renders: {rendered}"))
    };

    assert_eq!(the_value_input_rendered()["annotations"], annotations);
    let refusal = the_refusal_of_a_saved_input(
        &stream,
        &async_runtime,
        lines,
        "pin_type",
        json!({ "type": "boolean" }),
    );
    assert!(
        refusal.contains("states `range`") && refusal.contains("a Boolean pin does not take"),
        "{refusal}"
    );
    assert_eq!(the_value_input_rendered()["annotations"], annotations);
}

/// A playing `GraphInput` linked into a function node takes a saved change of its `control` and
/// its `unit` live: neither is refused nor leaves the node to rebuild, `graph` renders both on
/// its `value` input straight away, and the node goes on publishing its value.
///
/// Mental-revert: with the annotation settings read at setup, each change leaves the node to
/// rebuild, and a rebuild of a node linked to a function node is refused.
#[test]
#[serial]
fn a_playing_graph_input_linked_into_a_function_node_takes_a_change_of_its_annotations_live() {
    let project_directory = a_project_on_disk();
    let async_runtime = an_async_runtime();
    let lines = Uuid::new_v4();
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    stream
        .node_types_this_stream_resolves()
        .register_a_type_described_in_this_streams_node_interpreter(
            crate::core::compiler::compiler_ops::function_node_island::tests::a_function_node_type(
                "graph_input_annotation_tests::Caption",
                &["lines"],
                &["result"],
            ),
            crate::core::nodes::HowANodeOfADescribedTypeRuns::InsideTheReadOfEachNodeItFeeds {
                node_once_promoted_to_its_own_process: Box::new(|node| {
                    Err(Error::Configuration(format!(
                        "`{}` builds no node in this test",
                        node.id
                    )))
                }),
            },
        )
        .unwrap();
    let document = json!({
        "nodes": [
            a_graph_input_in_a_document(
                lines,
                "lines",
                json!({
                    "port": "lines", "pin_type": { "type": "integer" }, "value": 2,
                    "range": [1, 8], "control": "dial",
                }),
            ),
            { "name": "caption", "type": "graph_input_annotation_tests::Caption" },
        ],
        "links": [{
            "source": { "node": "lines", "port": VALUE },
            "target": { "node": "caption", "port": "lines" },
        }],
    });
    stream
        .load_graph_snapshot_into_this_stream(
            &GraphSnapshot::from_graph_document(document).unwrap(),
            None,
        )
        .expect("the graph loads");
    play(&stream);

    set_the_saved_input(&stream, &async_runtime, lines, "control", json!("slider"));
    set_the_saved_input(&stream, &async_runtime, lines, "unit", json!("lines"));
    let rebuilt = async_runtime
        .block_on(RuntimeOperations::rebuild_each_node_a_setting_read_at_setup_changed(&*stream))
        .expect("nothing is left to rebuild");

    assert_eq!(rebuilt, Vec::<Uuid>::new());
    let rendered = the_rendered_node(&stream, lines);
    let value_input = rendered["ports"]["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|input| input["name"] == VALUE)
        .unwrap_or_else(|| panic!("`value` renders: {rendered}"));
    assert_eq!(
        value_input["annotations"],
        json!({
            "range": { "minimum": 1.0, "maximum": 8.0 }, "unit": "lines", "control": "slider",
        }),
        "{rendered}"
    );
    let integer = json!({ "type": "integer" });
    let (_viewer, mut reader) = a_reader_of_the_value_of(&stream, &async_runtime, lines);
    assert_eq!(the_typed_value_read(&mut reader, &integer), json!(2));
    stream.stop().expect("the stream ends");
}
