// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A stream's graph inputs: the `value` input of each top-level `GraphInput`, a setting read
//! each run, set live or saved like any input, published on its `value` output at Play and on
//! each change, and kept or reset across a Stop, a Play and a restart as its `lifetime` says.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use serial_test::serial;
use streamlib_ipc_types::{
    ArrayFrameAttributeResidency, ArrayFrameDescriptorBody, DataChannelPayloadDescriptorKind,
    MessageReadFromAReservedSlot, OutputPortViewerReader, array_frame_attribute_table_of,
};
use streamlib_project_repository::{
    PROJECT_FILE_NAME, ProjectStore, ProjectStoreTableName, StreamGraphOperation,
    TatolabProjectFile,
};
use uuid::Uuid;

use crate::core::context::{LIVE_INPUT_VALUES_TABLE, key_of_the_node};
use crate::core::descriptors::PortFamily;
use crate::core::error::Error;
use crate::core::graph::graph_interface_resolution::GRAPH_INPUT_NODE_TYPE;
use crate::core::graph::graph_snapshot::GraphSnapshot;
use crate::core::graph::input_values::{
    InputValueToSetLive, InputValuesSetLive, LiveInputValueSetRefusal,
};
use crate::core::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
use crate::core::runtime::{
    LoadedStreamInThisRuntime, OutputPortViewerHeldOpen, Runner, RuntimeOperations,
};
use crate::core::pin_value_packing::{PackedPinValueRef, unpack_pin_value};
use crate::core::test_support::{
    MockOutputOnlyNode, MockPinTypedNode, a_temporary_directory_at_owner_only_mode,
    ensure_test_mocks_registered,
};
use crate::core::{InputLinkPortRef, NodeRunId, OutputLinkPortRef};

mod graph_input_relink_lifetime_and_range_tests;

const ARRIVES_WITHIN: Duration = Duration::from_secs(5);

/// The input of a `GraphInput` its value is set on, and the output it is published on.
const VALUE: &str = "value";

fn a_project_on_disk() -> tempfile::TempDir {
    let project_directory = a_temporary_directory_at_owner_only_mode().unwrap();
    std::fs::write(
        project_directory.path().join(PROJECT_FILE_NAME),
        TatolabProjectFile::of_a_new_project("acme-show")
            .canonical_bytes()
            .unwrap(),
    )
    .unwrap();
    project_directory
}

fn an_async_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

/// A project `acme-show` on disk, its stream `a` loaded in a runner of its own.
fn a_runner_holding_the_stream_of_the_project_at(
    project_directory: &Path,
) -> (Arc<Runner>, Arc<LoadedStreamInThisRuntime>) {
    ensure_test_mocks_registered();
    let runner = Runner::new().expect("the engine builds");
    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory,
            "a",
        ))
        .expect("the stream loads");
    (runner, stream)
}

/// A top-level `GraphInput` named `name` with the UUID `node`, as a stream document holds it.
fn a_graph_input_in_a_document(node: Uuid, name: &str, input_values: Value) -> Value {
    json!({ "id": node, "name": name, "type": GRAPH_INPUT_NODE_TYPE, "input_values": input_values })
}

fn load_the_graph_inputs(stream: &LoadedStreamInThisRuntime, graph_inputs: Vec<Value>) {
    stream
        .load_graph_snapshot_into_this_stream(
            &GraphSnapshot::from_graph_document(json!({ "nodes": graph_inputs })).unwrap(),
            None,
        )
        .expect("the graph loads");
}

fn play(stream: &Arc<LoadedStreamInThisRuntime>) {
    stream.start().expect("the stream plays");
    stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("the GraphInput runs");
}

fn a_reader_of_the_value_of(
    stream: &LoadedStreamInThisRuntime,
    async_runtime: &tokio::runtime::Runtime,
    node: Uuid,
) -> (OutputPortViewerHeldOpen, OutputPortViewerReader) {
    let viewer = async_runtime
        .block_on(stream.open_a_viewer_of_an_output_port(node.to_string(), VALUE.to_string(), None))
        .expect("a running GraphInput's value opens a viewer");
    let hand_off = viewer.hand_off().clone();
    let reader = OutputPortViewerReader::subscribe(
        &hand_off.iceoryx2_domain_root,
        &hand_off.channel_service_name,
        &hand_off.wake_service_name,
        "a-reader",
        hand_off.viewer_reads,
    )
    .expect("the reader subscribes to the port's channel");
    (viewer, reader)
}

fn the_next_message_read(reader: &mut OutputPortViewerReader) -> MessageReadFromAReservedSlot {
    let gave_up_at = Instant::now() + ARRIVES_WITHIN;
    loop {
        if let Some(message) = reader.read_the_latest_message().expect("the reader reads") {
            return message;
        }
        assert!(Instant::now() < gave_up_at, "no message reached the reader");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The numbers the one attribute of the array frame `message` holds.
fn the_numbers_an_array_frame_holds(message: &MessageReadFromAReservedSlot) -> Vec<f64> {
    let rows = array_frame_attribute_table_of(
        ArrayFrameDescriptorBody::from_descriptor_body(&message.payload_descriptor.body),
        &message.payload,
    )
    .expect("an array frame reads");
    let ArrayFrameAttributeResidency::Cpu {
        payload_byte_offset,
        byte_len,
    } = rows[0].residency
    else {
        panic!("a CPU attribute");
    };
    message.payload[payload_byte_offset as usize..(payload_byte_offset + byte_len) as usize]
        .as_chunks::<8>()
        .0
        .iter()
        .map(|element| f64::from_le_bytes(*element))
        .collect()
}

fn the_value_read(reader: &mut OutputPortViewerReader) -> Value {
    rmp_serde::from_slice(&the_next_message_read(reader).payload).unwrap()
}

/// Read `reader` until it reads `expected`, failing when it does not within the wait.
fn read_until_it_reads(reader: &mut OutputPortViewerReader, expected: &Value) {
    let gave_up_at = Instant::now() + ARRIVES_WITHIN;
    loop {
        if let Some(message) = reader.read_the_latest_message().expect("the reader reads") {
            let read_back: Value = rmp_serde::from_slice(&message.payload).unwrap();
            if &read_back == expected {
                return;
            }
        }
        assert!(
            Instant::now() < gave_up_at,
            "{expected} never reached the reader"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn the_value_a_new_reader_is_handed(
    stream: &LoadedStreamInThisRuntime,
    async_runtime: &tokio::runtime::Runtime,
    node: Uuid,
) -> Value {
    let (_viewer, mut reader) = a_reader_of_the_value_of(stream, async_runtime, node);
    the_value_read(&mut reader)
}

fn set_live(
    stream: &LoadedStreamInThisRuntime,
    node: Uuid,
    value: Value,
) -> std::result::Result<InputValuesSetLive, LiveInputValueSetRefusal> {
    stream.set_input_values_live(&[InputValueToSetLive {
        member: None,
        node,
        input: VALUE.to_owned(),
        value: Some(value),
    }])
}

fn set_the_saved_input(
    stream: &LoadedStreamInThisRuntime,
    async_runtime: &tokio::runtime::Runtime,
    node: Uuid,
    input: &str,
    value: Value,
) {
    async_runtime
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
}

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

fn the_rendered_node(stream: &LoadedStreamInThisRuntime, node: Uuid) -> Value {
    let rendered = stream.to_json().unwrap();
    rendered["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|rendered_node| rendered_node["id"] == node.to_string())
        .unwrap_or_else(|| panic!("`graph` renders {node}: {rendered}"))
        .clone()
}

/// The value the `GraphInput` `node` saves on its `value` input, beside its settings.
fn the_saved_value_of(stream: &LoadedStreamInThisRuntime, node: Uuid) -> Option<Value> {
    stream.compiler.scope_reading(|graph| {
        graph
            .node_by_uuid(node)
            .expect("the graph holds it")
            .pins
            .get("value")
            .cloned()
    })
}

fn the_live_value_of(stream: &LoadedStreamInThisRuntime, node: Uuid) -> Option<Value> {
    stream
        .compiler
        .scope_reading(|graph| graph.live_input_values().value_of(node, VALUE))
}

fn the_project_store_of(
    stream: &LoadedStreamInThisRuntime,
    project_directory: &Path,
) -> Arc<ProjectStore> {
    stream
        .engine_resources_shared_by_every_stream()
        .project_stores_open_in_this_runtime
        .the_store_of_the_project_at(project_directory)
        .expect("the project's store opens")
}

fn the_value_the_project_store_keeps_live(
    project_store: &ProjectStore,
    node: Uuid,
) -> Option<Value> {
    project_store
        .read(LIVE_INPUT_VALUES_TABLE, &key_of_the_node(node, VALUE))
        .unwrap()
}

/// A value set live on a `GraphInput`'s `value` input reaches each reader of its `value` output,
/// and `graph` renders it as the value the input reads while the saved value stays the
/// document's `default`.
///
/// Mental-revert: a node that does not publish the values it is handed leaves both readers at
/// the `default`.
#[test]
#[serial]
fn a_value_set_live_on_a_graph_input_reaches_each_reader_and_leaves_the_saved_value() {
    let project_directory = a_project_on_disk();
    let async_runtime = an_async_runtime();
    let cue = Uuid::new_v4();
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    load_the_graph_inputs(
        &stream,
        vec![a_graph_input_in_a_document(
            cue,
            "cue",
            json!({ "port": "cue", "family": "records", "default": { "track": "intro" } }),
        )],
    );
    play(&stream);
    let (_left_viewer, mut left_reader) = a_reader_of_the_value_of(&stream, &async_runtime, cue);
    let (_right_viewer, mut right_reader) = a_reader_of_the_value_of(&stream, &async_runtime, cue);

    for record in [json!({ "track": "drop" }), json!({ "track": "outro" })] {
        assert!(matches!(
            set_live(&stream, cue, record.clone()).expect("the value is set live"),
            InputValuesSetLive::ApartFromTheGraph(_)
        ));
        for reader in [&mut left_reader, &mut right_reader] {
            read_until_it_reads(reader, &record);
        }
    }

    let rendered = the_rendered_node(&stream, cue);
    assert_eq!(
        rendered["components"]["graph_interface"]["current_value"],
        json!({ "track": "outro" }),
        "{rendered}"
    );
    assert_eq!(
        rendered["config"]["default"],
        json!({ "track": "intro" }),
        "{rendered}"
    );
    assert_eq!(
        the_saved_value_of(&stream, cue),
        Some(json!({ "track": "intro" }))
    );
    stream.stop().expect("the stream ends");
}

/// A document's `default`, a `GraphInput`'s `value` before it was an input, opens as the saved
/// value of `value`, which a reader opened after the Play is handed with no further publish, and
/// a saved value set later reaches it and renders as the `default`.
///
/// Mental-revert: publishing the start value on a schedule rather than keeping it for a late
/// reader leaves it reading nothing until the schedule fires, and one that never fires leaves it
/// reading nothing at all.
#[test]
#[serial]
fn a_graph_inputs_saved_value_reaches_a_reader_opened_after_it_was_published() {
    let project_directory = a_project_on_disk();
    let async_runtime = an_async_runtime();
    let fader = Uuid::new_v4();
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    load_the_graph_inputs(
        &stream,
        vec![a_graph_input_in_a_document(
            fader,
            "fader",
            json!({ "port": "fader", "family": "records", "default": { "level": 0.5 } }),
        )],
    );
    assert_eq!(
        the_saved_value_of(&stream, fader),
        Some(json!({ "level": 0.5 }))
    );
    play(&stream);
    assert_eq!(
        the_value_a_new_reader_is_handed(&stream, &async_runtime, fader),
        json!({ "level": 0.5 })
    );

    set_the_saved_input(
        &stream,
        &async_runtime,
        fader,
        VALUE,
        json!({ "level": 0.75 }),
    );

    assert_eq!(
        the_value_a_new_reader_is_handed(&stream, &async_runtime, fader),
        json!({ "level": 0.75 })
    );
    let rendered = the_rendered_node(&stream, fader);
    assert_eq!(
        rendered["config"]["default"],
        json!({ "level": 0.75 }),
        "{rendered}"
    );
    assert_eq!(
        rendered["input_values"][VALUE],
        json!({ "level": 0.75 }),
        "a saved edit renders on the `value` input the `default` its config renders: {rendered}"
    );
    let value_input = rendered["ports"]["inputs"]
        .as_array()
        .and_then(|inputs| inputs.iter().find(|input| input["name"] == VALUE))
        .unwrap_or_else(|| panic!("the running `GraphInput` keeps its `value` input: {rendered}"));
    assert_eq!(value_input["value"], json!({ "level": 0.75 }), "{rendered}");
    assert_eq!(value_input["value_comes_from"], "saved", "{rendered}");
    assert_eq!(
        rendered["components"]["graph_interface"]["current_value"],
        json!({ "level": 0.75 }),
        "{rendered}"
    );
    stream.stop().expect("the stream ends");
}

/// An array set on a `GraphInput` reaches its reader as an array frame.
#[test]
#[serial]
fn an_array_set_on_a_graph_input_reaches_its_reader_as_an_array_frame() {
    let project_directory = a_project_on_disk();
    let async_runtime = an_async_runtime();
    let fader = Uuid::new_v4();
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    load_the_graph_inputs(
        &stream,
        vec![a_graph_input_in_a_document(
            fader,
            "fader",
            json!({ "port": "Fader", "family": "array", "range": [0, 1], "default": 0 }),
        )],
    );
    play(&stream);
    let (_viewer, mut reader) = a_reader_of_the_value_of(&stream, &async_runtime, fader);

    set_live(&stream, fader, json!(0.25)).expect("the value is set live");

    let gave_up_at = Instant::now() + ARRIVES_WITHIN;
    loop {
        if let Some(message) = reader.read_the_latest_message().expect("the reader reads") {
            assert_eq!(
                message.payload_descriptor.known_kind().ok(),
                Some(DataChannelPayloadDescriptorKind::ArrayFrame)
            );
            if the_numbers_an_array_frame_holds(&message) == [0.25] {
                break;
            }
        }
        assert!(Instant::now() < gave_up_at, "0.25 never reached the reader");
        std::thread::sleep(Duration::from_millis(5));
    }
    stream.stop().expect("the stream ends");
}

/// A value the family of a `GraphInput` does not take, or a number outside its `range`, is
/// refused naming why, and leaves the value it reads as it was.
#[test]
#[serial]
fn a_value_a_graph_input_does_not_carry_is_refused_naming_why() {
    let project_directory = a_project_on_disk();
    let cue = Uuid::new_v4();
    let fader = Uuid::new_v4();
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    load_the_graph_inputs(
        &stream,
        vec![
            a_graph_input_in_a_document(cue, "cue", json!({ "port": "cue", "family": "records" })),
            a_graph_input_in_a_document(
                fader,
                "fader",
                json!({ "port": "fader", "family": "array", "range": [0, 1] }),
            ),
        ],
    );
    play(&stream);

    for (node, value, words) in [
        (cue, json!("drop"), ["carries Records", "JSON object"]),
        (fader, json!(2), ["`fader`", "from 0 to 1"]),
    ] {
        match set_live(&stream, node, value) {
            Err(LiveInputValueSetRefusal::TheInputDoesNotTakeIt { refusal, .. }) => {
                assert!(
                    words.iter().all(|word| refusal.words.contains(word)),
                    "{}",
                    refusal.words
                );
            }
            other => panic!("the value is refused: {other:?}"),
        }
        assert_eq!(the_live_value_of(&stream, node), None);
    }
    stream.stop().expect("the stream ends");
}

/// A stream's input is found by the port its top-level `GraphInput` declares, compared as an
/// exposed name is cast; one none declares is refused naming the inputs the stream has.
#[test]
#[serial]
fn a_stream_input_is_found_by_the_port_its_graph_input_declares() {
    let project_directory = a_project_on_disk();
    let async_runtime = an_async_runtime();
    let cue = Uuid::new_v4();
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    load_the_graph_inputs(
        &stream,
        vec![a_graph_input_in_a_document(
            cue,
            "cue",
            json!({ "port": "Cue", "family": "records" }),
        )],
    );
    let found_before_the_play = async_runtime
        .block_on(stream.read_the_top_level_graph_input_declaring("cue".to_string()))
        .expect("the input is found");
    assert_eq!(found_before_the_play.node, cue);
    assert_eq!(found_before_the_play.family, PortFamily::Records);
    assert!(
        found_before_the_play
            .refusal_while_its_node_does_not_run
            .is_some()
    );
    play(&stream);
    assert!(
        async_runtime
            .block_on(stream.read_the_top_level_graph_input_declaring("cue".to_string()))
            .expect("the input is found")
            .refusal_while_its_node_does_not_run
            .is_none()
    );

    match async_runtime
        .block_on(stream.read_the_top_level_graph_input_declaring("fader".to_string()))
        .expect_err("an input no GraphInput declares is refused")
    {
        Error::StreamHasNoInputNamed {
            input,
            inputs_the_stream_has,
            ..
        } => {
            assert_eq!(input, "fader");
            assert_eq!(inputs_the_stream_has, ["Cue"]);
        }
        other => panic!("the refusal names the stream's inputs: {other}"),
    }
    stream.stop().expect("the stream ends");
}

/// A `kept` `GraphInput`'s live value is handed to a new reader after a Stop and a Play, and
/// after a restart of the runtime, from the project's store.
///
/// Mental-revert: a `GraphInput` whose `value` follows the lifetime of a node type's input,
/// rather than its own `lifetime`, keeps nothing in the store and starts from its `default`.
#[test]
#[serial]
fn a_kept_graph_input_reaches_a_new_reader_with_its_value_after_a_stop_play_and_restart() {
    let project_directory = a_project_on_disk();
    let async_runtime = an_async_runtime();
    let lights = Uuid::new_v4();
    let the_lights_input = || {
        a_graph_input_in_a_document(
            lights,
            "lights",
            json!({ "port": "lights", "family": "records", "default": { "level": 0.0 } }),
        )
    };
    let written = json!({ "level": 0.8, "scene": "warm" });
    {
        let (_runner, stream) =
            a_runner_holding_the_stream_of_the_project_at(project_directory.path());
        load_the_graph_inputs(&stream, vec![the_lights_input()]);
        play(&stream);
        set_live(&stream, lights, written.clone()).expect("the value is set live");
        stream
            .stop_playback_keeping_the_graph()
            .expect("the stream stops");
        play(&stream);
        assert_eq!(
            the_value_a_new_reader_is_handed(&stream, &async_runtime, lights),
            written
        );
        stream.stop().expect("the stream ends");
        the_project_store_of(&stream, project_directory.path())
            .commit_every_waiting_write_now()
            .expect("the store commits");
    }
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    load_the_graph_inputs(&stream, vec![the_lights_input()]);
    play(&stream);
    assert_eq!(
        the_value_a_new_reader_is_handed(&stream, &async_runtime, lights),
        written
    );
    stream.stop().expect("the stream ends");
}

/// A `reset` `GraphInput` starts from its saved value after a Stop, and refuses a live value
/// while stopped, naming the stopped playback.
#[test]
#[serial]
fn a_reset_graph_input_starts_from_its_saved_value_after_a_stop() {
    let project_directory = a_project_on_disk();
    let async_runtime = an_async_runtime();
    let trigger = Uuid::new_v4();
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    load_the_graph_inputs(
        &stream,
        vec![a_graph_input_in_a_document(
            trigger,
            "trigger",
            json!({ "port": "trigger", "family": "records", "default": { "on": false },
                    "lifetime": "reset" }),
        )],
    );
    play(&stream);
    set_live(&stream, trigger, json!({ "on": true })).expect("the value is set live");
    stream
        .stop_playback_keeping_the_graph()
        .expect("the stream stops");

    assert!(matches!(
        set_live(&stream, trigger, json!({ "on": true })),
        Err(LiveInputValueSetRefusal::TheInputIsResetWhileStopped { .. })
    ));
    play(&stream);
    assert_eq!(
        the_value_a_new_reader_is_handed(&stream, &async_runtime, trigger),
        json!({ "on": false })
    );
    stream.stop().expect("the stream ends");
}

/// The value a top-level `GraphInput` kept in its own store before its value was an input is
/// carried, once, into the live values of the project's store as the stream loads: the input
/// reads it, a reader is handed it, and the old key is gone.
///
/// Mental-revert: no carry leaves the input at its `default` and the old key in place.
#[test]
#[serial]
fn the_value_a_graph_input_kept_under_its_old_key_is_carried_into_its_live_value() {
    let project_directory = a_project_on_disk();
    let async_runtime = an_async_runtime();
    let lights = Uuid::new_v4();
    let kept_before = json!({ "level": 0.4 });
    let old_key = format!("{lights}/value");
    let node_store = ProjectStoreTableName::named("node_store");
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    let project_store = the_project_store_of(&stream, project_directory.path());
    project_store
        .write(node_store, &old_key, &kept_before)
        .unwrap();

    load_the_graph_inputs(
        &stream,
        vec![a_graph_input_in_a_document(
            lights,
            "lights",
            json!({ "port": "lights", "family": "records", "default": { "level": 0.0 } }),
        )],
    );

    assert_eq!(
        the_live_value_of(&stream, lights),
        Some(kept_before.clone())
    );
    assert_eq!(
        the_value_the_project_store_keeps_live(&project_store, lights),
        Some(kept_before.clone())
    );
    assert_eq!(project_store.read(node_store, &old_key).unwrap(), None);
    play(&stream);
    assert_eq!(
        the_value_a_new_reader_is_handed(&stream, &async_runtime, lights),
        kept_before
    );
    stream.stop().expect("the stream ends");
}

/// A running `GraphInput` whose `lifetime` changes holds its live value under the new one: kept
/// in the project's store once `kept`, forgotten there once `reset`.
#[test]
#[serial]
fn a_running_graph_input_takes_a_new_lifetime_from_a_change_of_its_config() {
    let project_directory = a_project_on_disk();
    let async_runtime = an_async_runtime();
    let fader = Uuid::new_v4();
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    load_the_graph_inputs(
        &stream,
        vec![a_graph_input_in_a_document(
            fader,
            "fader",
            json!({ "port": "fader", "family": "array", "default": 0.0, "lifetime": "reset" }),
        )],
    );
    play(&stream);
    let project_store = the_project_store_of(&stream, project_directory.path());
    set_live(&stream, fader, json!(0.3)).expect("the value is set live");
    assert_eq!(
        the_value_the_project_store_keeps_live(&project_store, fader),
        None
    );

    set_the_saved_input(&stream, &async_runtime, fader, "lifetime", json!("kept"));
    assert_eq!(
        the_value_the_project_store_keeps_live(&project_store, fader),
        Some(json!(0.3))
    );
    set_live(&stream, fader, json!(0.6)).expect("the value is set live");
    assert_eq!(
        the_value_the_project_store_keeps_live(&project_store, fader),
        Some(json!(0.6))
    );

    set_the_saved_input(&stream, &async_runtime, fader, "lifetime", json!("reset"));
    assert_eq!(
        the_value_the_project_store_keeps_live(&project_store, fader),
        None
    );
    assert_eq!(the_live_value_of(&stream, fader), Some(json!(0.6)));
    stream.stop().expect("the stream ends");
}

#[test]
#[serial]
fn a_graph_input_declaring_a_lifetime_that_is_neither_kept_nor_reset_is_refused() {
    let refusal =
        crate::core::graph::graph_interface_resolution::the_value_lifetime_a_graph_input_config_declares(
            Some(&json!({ "lifetime": "forever" })),
        )
        .unwrap_err();
    assert!(
        refusal.contains("\"forever\"") && refusal.contains("\"kept\""),
        "{refusal}"
    );
}

/// A published Studio reads a node's values from the keys `graph` rendered before a node held
/// one map of input values, so a stream saved in the one map renders that map as
/// `input_values` and still renders those keys, derived from it: a node's `config`, `pins` and
/// `exposed_config_fields`, a port's `family` and `pin_default`, a `GraphInput`'s
/// `graph_interface.current_value`, and a node type's `config_schema`.
#[test]
#[serial]
fn a_stream_saved_with_one_map_of_input_values_renders_the_keys_a_published_studio_reads() {
    let project_directory = a_temporary_directory_at_owner_only_mode().unwrap();
    std::fs::write(
        project_directory.path().join(PROJECT_FILE_NAME),
        TatolabProjectFile::of_a_new_project("acme-show")
            .canonical_bytes()
            .unwrap(),
    )
    .unwrap();
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    let source_type =
        crate::core::test_support::MockSourceTakingOneSetting::node_class_import_path();
    let sink_type = crate::core::test_support::MockInputOnlyNode::node_class_import_path();
    let graph = crate::core::graph::graph_snapshot::GraphSnapshot::from_graph_document(json!({
        "nodes": [
            {"name": "front", "type": source_type.as_str(),
             "input_values": {"frame_width": 640}},
            {"name": "sink", "type": sink_type.as_str(), "input_values": {"in1": {"level": 1}}},
            {"name": "lights",
             "type": crate::core::graph::graph_interface_resolution::GRAPH_INPUT_NODE_TYPE,
             "input_values": {"port": "lights", "family": "records",
                              "default": {"level": 0.0}}},
        ]
    }))
    .unwrap();
    stream
        .load_graph_snapshot_into_this_stream(&graph, None)
        .expect("the graph loads");

    let rendered = stream.to_json().unwrap();
    let node_named = |name: &str| {
        rendered["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["name"] == name)
            .unwrap_or_else(|| panic!("`graph` renders {name}: {rendered}"))
            .clone()
    };
    let front = node_named("front");
    assert_eq!(front["input_values"], json!({"frame_width": 640}));
    assert_eq!(front["config"], json!({"frame_width": 640}));
    let sink = node_named("sink");
    assert_eq!(sink["input_values"], json!({"in1": {"level": 1}}));
    assert_eq!(sink["pins"], json!({"in1": {"level": 1}}));
    assert_eq!(
        sink["ports"]["inputs"][0]["family"]["family"],
        json!("records")
    );
    let lights = node_named("lights");
    assert_eq!(
        lights["input_values"],
        json!({"port": "lights", "family": "records", "value": {"level": 0.0}}),
        "a `GraphInput` holds its saved value on its `value` input"
    );
    assert_eq!(
        lights["config"],
        json!({"port": "lights", "family": "records", "default": {"level": 0.0}})
    );
    assert_eq!(
        lights["components"]["graph_interface"]["current_value"],
        json!({"level": 0.0})
    );
    let properties_of = |component: &str| {
        crate::core::json_schema::the_control_contract_schema_named(component)["properties"].clone()
    };
    for key in ["config", "pins", "exposed_config_fields", "components"] {
        assert!(
            properties_of("GraphNodeOnTheWire").get(key).is_some(),
            "`graph` renders `{key}` on a node"
        );
    }
    for key in ["family", "pin_default"] {
        assert!(
            properties_of("GraphNodePortOnTheWire").get(key).is_some(),
            "`graph` renders `{key}` on a port"
        );
    }
    let catalog = serde_json::to_value(schemars::schema_for!(
        crate::core::json_schema::NodeDescriptorOutput
    ))
    .unwrap();
    assert!(
        catalog["properties"].get("config_schema").is_some(),
        "the catalog renders each type's `config_schema`: {catalog}"
    );
    stream.stop().expect("the stream ends");
}

/// A value the project's store keeps live for a `GraphInput` whose family no longer takes it is
/// dropped as the stream loads: the input reads its saved value, a reader is handed that, and
/// the store keeps nothing for it.
///
/// Mental-revert: holding every kept value without asking the input whether it takes it leaves
/// the string live on an array input.
#[test]
#[serial]
fn a_kept_value_a_graph_inputs_family_no_longer_takes_is_dropped_at_load() {
    let project_directory = a_project_on_disk();
    let async_runtime = an_async_runtime();
    let fader = Uuid::new_v4();
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    let project_store = the_project_store_of(&stream, project_directory.path());
    project_store
        .write(
            LIVE_INPUT_VALUES_TABLE,
            &key_of_the_node(fader, VALUE),
            &json!("not a number"),
        )
        .unwrap();

    load_the_graph_inputs(
        &stream,
        vec![a_graph_input_in_a_document(
            fader,
            "fader",
            json!({ "port": "fader", "family": "array", "default": 0.25 }),
        )],
    );

    assert_eq!(the_live_value_of(&stream, fader), None);
    assert_eq!(
        the_value_the_project_store_keeps_live(&project_store, fader),
        None
    );
    play(&stream);
    let (_viewer, mut reader) = a_reader_of_the_value_of(&stream, &async_runtime, fader);
    let message = the_next_message_read(&mut reader);
    assert_eq!(
        message.payload_descriptor.known_kind().ok(),
        Some(DataChannelPayloadDescriptorKind::ArrayFrame)
    );
    stream.stop().expect("the stream ends");
}

/// A value set live on a `kept` `GraphInput` while its stream is stopped renders as the value it
/// reads, and reaches a reader at the next Play.
///
/// Mental-revert: refusing a live set while stopped, as a `reset` input is refused, leaves the
/// input at its `default` at the Play.
#[test]
#[serial]
fn a_value_set_live_on_a_stopped_kept_graph_input_renders_and_reaches_the_next_play() {
    let project_directory = a_project_on_disk();
    let async_runtime = an_async_runtime();
    let lights = Uuid::new_v4();
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    load_the_graph_inputs(
        &stream,
        vec![a_graph_input_in_a_document(
            lights,
            "lights",
            json!({ "port": "lights", "family": "records", "default": { "level": 0.0 } }),
        )],
    );
    play(&stream);
    stream
        .stop_playback_keeping_the_graph()
        .expect("the stream stops");
    let written = json!({ "level": 0.6 });

    set_live(&stream, lights, written.clone()).expect("a kept input takes a value while stopped");

    let rendered = the_rendered_node(&stream, lights);
    assert_eq!(
        rendered["components"]["graph_interface"]["current_value"], written,
        "{rendered}"
    );
    play(&stream);
    assert_eq!(
        the_value_a_new_reader_is_handed(&stream, &async_runtime, lights),
        written
    );
    stream.stop().expect("the stream ends");
}

/// The run id of the node `node` of `stream`.
fn the_run_id_of(stream: &LoadedStreamInThisRuntime, node: Uuid) -> NodeRunId {
    stream.compiler.scope_reading(|graph| {
        graph
            .node_by_uuid(node)
            .expect("the graph holds it")
            .id
            .clone()
    })
}

/// A link into the `value` of a `GraphInput`, a setting, is refused naming it, and the graph
/// holds no link.
///
/// Mental-revert: without the rule refusing a link into a setting, the link is made.
#[test]
#[serial]
fn a_link_into_the_value_of_a_graph_input_is_refused() {
    let project_directory = a_project_on_disk();
    let cue = Uuid::new_v4();
    let echo = Uuid::new_v4();
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    load_the_graph_inputs(
        &stream,
        vec![
            a_graph_input_in_a_document(cue, "cue", json!({ "port": "cue", "family": "records" })),
            a_graph_input_in_a_document(
                echo,
                "echo",
                json!({ "port": "echo", "family": "records" }),
            ),
        ],
    );

    let refusal = stream
        .connect(
            OutputLinkPortRef::new(the_run_id_of(&stream, cue), VALUE),
            InputLinkPortRef::new(the_run_id_of(&stream, echo), VALUE),
        )
        .expect_err("a link into a setting is refused")
        .to_string();

    assert!(
        refusal.contains("input `value` of `echo`, a setting, which takes no wire"),
        "{refusal}"
    );
    assert_eq!(stream.to_json().unwrap()["links"], json!([]));
}

/// A stream document holding a link into the `value` of a top-level `GraphInput` is refused as
/// it is validated, naming the setting, before the load adds any node.
///
/// Mental-revert: without the setting rule in the document's validation, the link is refused at
/// its connect, after the load added the graph's nodes.
#[test]
#[serial]
fn a_document_linking_into_the_value_of_a_graph_input_is_refused_at_load() {
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
            "target": { "node": "echo", "port": VALUE },
        }],
    });

    let refusal = stream
        .load_graph_snapshot_into_this_stream(
            &GraphSnapshot::from_graph_document(document).unwrap(),
            None,
        )
        .expect_err("a document linking into a setting is refused")
        .to_string();

    assert!(
        refusal.contains("input `value` of `echo`, a setting, which takes no wire"),
        "{refusal}"
    );
    assert_eq!(stream.node_count(), 0, "the refused load added no node");
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
        rendered["components"]["graph_interface"]["pin_type"],
        float,
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
