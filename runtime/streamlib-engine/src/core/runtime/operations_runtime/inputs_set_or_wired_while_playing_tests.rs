// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! An input of a playing node, set, wired, unwired or wired again, is handed to the node where
//! it runs: its setup runs once, at the stream's start, and the stream stays started. A burst a
//! wire carries ends on its last value, and a wire taken off hands the node its set value back.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use serial_test::serial;
use streamlib_project_repository::StreamGraphOperation;
use uuid::Uuid;

use super::LoadedStreamInThisRuntime;
use crate::core::graph::{InputLinkPortRef, LinkUniqueId, NodeRunId, OutputLinkPortRef};
use crate::core::nodes::NodeSpec;
use crate::core::runtime::a_node_reading_its_settings_at_setup_for_a_test::{
    A_KERNEL_SIZE_IT_IS_NOT_HANDED, THE_LAYERED_NODES_PIN_GROUP, THE_SOURCES_OUTPUT,
    forget_every_setup_run, the_node_type_built_for_its_layers,
    the_node_type_reading_its_settings_at_setup, the_node_type_whose_output_a_test_writes,
    the_settings_each_setup_read, the_values_last_handed_to, write_from_the_source,
};
use crate::core::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
use crate::core::runtime::operations::RuntimeOperations;
use crate::core::runtime::{Runner, RuntimeStatus};

/// The longest a test waits for a node to be handed a value.
const A_VALUE_IS_HANDED_WITHIN: Duration = Duration::from_secs(10);

/// A playing stream holding a source whose Integer output a test writes and a node reading
/// `kernel_size`, stored as 3.
struct APlayingStream {
    _runner: Arc<Runner>,
    stream: Arc<LoadedStreamInThisRuntime>,
    source: NodeRunId,
    source_uuid: Uuid,
    reader: NodeRunId,
    reader_uuid: Uuid,
}

impl APlayingStream {
    fn wire_the_source_into_the_kernel_size(&self) -> LinkUniqueId {
        self.stream
            .connect(
                OutputLinkPortRef::new(self.source.clone(), THE_SOURCES_OUTPUT),
                InputLinkPortRef::new(self.reader.clone(), "kernel_size"),
            )
            .expect("a Value wire into the kernel size connects")
    }

    fn write(&self, value: i64) {
        write_from_the_source(self.source_uuid, value).expect("the source writes");
    }

    /// The kernel size the reader is handed once a write of `value` from the source, repeated
    /// until a wire just connected is read, reaches it.
    fn write_until_handed(&self, value: i64) -> Option<Value> {
        let deadline = Instant::now() + A_VALUE_IS_HANDED_WITHIN;
        loop {
            self.write(value);
            let handed = the_kernel_size_once_handed_within(
                self.reader_uuid,
                &json!(value),
                Duration::from_millis(100),
            );
            if handed == Some(json!(value)) || Instant::now() >= deadline {
                return handed;
            }
        }
    }

    fn assert_set_up_once_and_started(&self) {
        let setups = the_settings_each_setup_read(self.reader_uuid);
        assert_eq!(setups.len(), 1, "one setup, at the start: {setups:?}");
        assert_eq!(self.stream.status(), RuntimeStatus::Started);
    }
}

fn a_playing_stream(project_directory: &std::path::Path) -> APlayingStream {
    let reader_type = the_node_type_reading_its_settings_at_setup();
    let source_type = the_node_type_whose_output_a_test_writes();
    forget_every_setup_run();
    let runner = Runner::new().expect("Runner::new");
    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory,
            "main",
        ))
        .expect("an empty stream loads");
    let (source_uuid, reader_uuid) = (Uuid::new_v4(), Uuid::new_v4());
    let source = stream
        .add_node(NodeSpec::new(source_type, Value::Null).with_node_uuid(source_uuid))
        .expect("the source is added");
    let reader = stream
        .add_node(NodeSpec::new(reader_type, Value::Null).with_node_uuid(reader_uuid))
        .expect("the reader is added");
    set(&stream, reader_uuid, "kernel_size", json!(3));
    stream.start().expect("the stream starts");
    stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("every node runs");
    APlayingStream {
        _runner: runner,
        stream,
        source,
        source_uuid,
        reader,
        reader_uuid,
    }
}

fn on_the_tests_own_runtime<T>(operation: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
        .block_on(operation)
}

fn set(stream: &LoadedStreamInThisRuntime, node: Uuid, input: &str, to: Value) {
    on_the_tests_own_runtime(stream.apply_a_stream_graph_operation_live(
        StreamGraphOperation::SetNodeInputValue {
            node,
            input: input.to_owned(),
            member: None,
            from: None,
            to: Some(to),
        },
    ))
    .unwrap_or_else(|refusal| panic!("`{input}` is set: {refusal}"));
}

/// The kernel size `node` was last handed once it is `expected`, or the last one handed.
fn the_kernel_size_once_handed(node: Uuid, expected: &Value) -> Option<Value> {
    the_kernel_size_once_handed_within(node, expected, A_VALUE_IS_HANDED_WITHIN)
}

fn the_kernel_size_once_handed_within(
    node: Uuid,
    expected: &Value,
    within: Duration,
) -> Option<Value> {
    let deadline = Instant::now() + within;
    loop {
        let handed =
            the_values_last_handed_to(node).and_then(|values| values.get("kernel_size").cloned());
        if handed.as_ref() == Some(expected) || Instant::now() >= deadline {
            return handed;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Mental-revert: leaving a node whose setting is set to be rebuilt runs its setup again.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_setting_set_while_playing_is_handed_to_its_node_with_no_setup() {
    let project_directory = tempfile::tempdir().unwrap();
    let playing = a_playing_stream(project_directory.path());

    set(
        &playing.stream,
        playing.reader_uuid,
        "kernel_size",
        json!(5),
    );

    assert_eq!(
        the_kernel_size_once_handed(playing.reader_uuid, &json!(5)),
        Some(json!(5))
    );
    playing.assert_set_up_once_and_started();
    playing.stream.stop().expect("the stream stops");
}

/// A wire into a setting hands its node each value it carries, a burst ending on its last; taken
/// off, it hands back the value set while it was wired; wired again, it hands the node what it
/// carries once more; and none of it runs the node's setup again or stops the stream.
///
/// Mental-revert: rebuilding the node for each value its wire carries runs its setup again;
/// forgetting the wire's value without handing the node its set value leaves it with 50.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn wiring_unwiring_and_rewiring_a_setting_while_playing_hands_its_node_each_value_with_no_setup() {
    let project_directory = tempfile::tempdir().unwrap();
    let playing = a_playing_stream(project_directory.path());
    let reader = playing.reader_uuid;

    let link = playing.wire_the_source_into_the_kernel_size();
    assert_eq!(playing.write_until_handed(7), Some(json!(7)));
    for value in 1..=50 {
        playing.write(value);
    }
    assert_eq!(
        the_kernel_size_once_handed(reader, &json!(50)),
        Some(json!(50)),
        "a burst ends on its last value"
    );
    set(&playing.stream, reader, "kernel_size", json!(4));
    playing.assert_set_up_once_and_started();

    playing
        .stream
        .disconnect(&link)
        .expect("the wire is taken off");
    assert_eq!(
        the_kernel_size_once_handed(reader, &json!(4)),
        Some(json!(4)),
        "the wire taken off hands back the value set"
    );

    playing.wire_the_source_into_the_kernel_size();
    assert_eq!(playing.write_until_handed(9), Some(json!(9)));
    playing.assert_set_up_once_and_started();
    playing.stream.stop().expect("the stream stops");
}

/// A value a wire carries that its node refuses leaves the node with the value it was last
/// handed, and the stream started.
///
/// Mental-revert: a refusal that stops the hand-over task hands the node nothing after it; one
/// that fails the stream leaves it no longer started.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_value_a_wire_carries_that_its_node_refuses_leaves_it_its_last_value_and_the_stream_started() {
    let project_directory = tempfile::tempdir().unwrap();
    let playing = a_playing_stream(project_directory.path());
    let reader = playing.reader_uuid;
    playing.wire_the_source_into_the_kernel_size();
    assert_eq!(playing.write_until_handed(7), Some(json!(7)));

    playing.write(A_KERNEL_SIZE_IT_IS_NOT_HANDED);
    std::thread::sleep(Duration::from_millis(300));

    assert_eq!(
        the_values_last_handed_to(reader).and_then(|values| values.get("kernel_size").cloned()),
        Some(json!(7))
    );
    playing.assert_set_up_once_and_started();
    assert_eq!(
        playing.write_until_handed(9),
        Some(json!(9)),
        "the next value is handed"
    );
    playing.stream.stop().expect("the stream stops");
}

/// Mental-revert: rebuilding a node whose pin group a link grows runs its setup again.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_link_growing_a_playing_nodes_pin_group_runs_no_setup() {
    let project_directory = tempfile::tempdir().unwrap();
    let playing = a_playing_stream(project_directory.path());
    let layered_uuid = Uuid::new_v4();
    let layered = playing
        .stream
        .add_node(
            NodeSpec::new(the_node_type_built_for_its_layers(), Value::Null)
                .with_node_uuid(layered_uuid),
        )
        .expect("the layered node is added");
    playing
        .stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("every node runs");

    let link = playing
        .stream
        .connect(
            OutputLinkPortRef::new(playing.source.clone(), THE_SOURCES_OUTPUT),
            InputLinkPortRef::new(layered, format!("{THE_LAYERED_NODES_PIN_GROUP}.0.layer")),
        )
        .expect("an Integer links into an Integer layer");
    playing
        .stream
        .disconnect(&link)
        .expect("the link is taken out");

    assert_eq!(the_settings_each_setup_read(layered_uuid).len(), 1);
    assert_eq!(playing.stream.status(), RuntimeStatus::Started);
    playing.stream.stop().expect("the stream stops");
}
