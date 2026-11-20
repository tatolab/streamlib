// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A Value wire into a setting a node reads at setup builds the node with the value its source
//! wrote as it set up, and rebuilds the node once with each value it carries while its stream
//! runs, a burst of them once with the last; a value the node does not build with leaves that
//! node, and no other, built with its old one; a wire taken off rebuilds its node with the
//! setting's own value; and the value the wire last carried is the one the node is built with
//! at the stream's next start.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use serial_test::serial;
use streamlib_project_repository::StreamGraphOperation;
use uuid::Uuid;

use super::LoadedStreamInThisRuntime;
use crate::core::graph::{InputLinkPortRef, LinkUniqueId, NodeRunId, OutputLinkPortRef};
use crate::core::nodes::NodeSpec;
use crate::core::runtime::Runner;
use crate::core::runtime::a_node_reading_its_settings_at_setup_for_a_test::{
    A_KERNEL_SIZE_IT_CANNOT_BUILD_WITH, A_KERNEL_SIZE_IT_IS_NOT_HANDED, THE_SOURCES_OUTPUT,
    THE_SOURCES_SETTING, WhatANodeRan, each_setup_and_teardown_run, fail_the_setup_of_the_source,
    forget_every_setup_run, the_node_type_reading_its_settings_at_setup,
    the_node_type_whose_output_a_test_writes, the_settings_each_setup_read, write_from_the_source,
    write_from_the_source_at_its_setup,
};
use crate::core::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;
use crate::core::runtime::operations::RuntimeOperations;

/// The longest a test waits for a rebuild a wire asked for.
const A_REBUILD_IS_RUN_WITHIN: Duration = Duration::from_secs(10);

/// Long enough for any rebuild a wire asked for to have run, whatever the window a burst
/// settles within.
const ANY_REBUILD_HAS_RUN_AFTER: Duration = Duration::from_millis(600);

/// A stream holding wires into settings, each from a source whose Integer output it carries
/// into the setting `kernel_size` of a node reading its settings at setup, stored as 3.
struct AStreamWithWiresIntoSettings {
    _runner: Arc<Runner>,
    stream: Arc<LoadedStreamInThisRuntime>,
    sources: Vec<Uuid>,
    readers: Vec<Uuid>,
    links: Vec<LinkUniqueId>,
}

/// A stream holding `wires` wires into settings, started after `before_start` is handed the
/// sources' UUIDs.
fn a_running_stream_with_wires_into_settings(
    project_directory: &std::path::Path,
    wires: usize,
    before_start: impl FnOnce(&[Uuid]),
) -> AStreamWithWiresIntoSettings {
    let wired = a_stream_with_wires_into_settings(project_directory, wires);
    before_start(&wired.sources);
    start(&wired.stream);
    wired
}

/// A stream holding `wires` wires into settings, not started.
fn a_stream_with_wires_into_settings(
    project_directory: &std::path::Path,
    wires: usize,
) -> AStreamWithWiresIntoSettings {
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
    let (mut sources, mut readers, mut links) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..wires {
        let source = Uuid::new_v4();
        let reader = Uuid::new_v4();
        let source_run_id: NodeRunId = stream
            .add_node(NodeSpec::new(source_type.clone(), Value::Null).with_node_uuid(source))
            .expect("the source is added");
        let reader_run_id = stream
            .add_node(NodeSpec::new(reader_type.clone(), Value::Null).with_node_uuid(reader))
            .expect("the reader is added");
        on_the_tests_own_runtime(stream.apply_a_stream_graph_operation_live(
            StreamGraphOperation::SetNodeInputValue {
                node: reader,
                input: "kernel_size".to_owned(),
                member: None,
                from: None,
                to: Some(json!(3)),
            },
        ))
        .expect("a stored kernel size is set");
        links.push(
            stream
                .connect(
                    OutputLinkPortRef::new(source_run_id, THE_SOURCES_OUTPUT),
                    InputLinkPortRef::new(reader_run_id, "kernel_size"),
                )
                .expect("a Value wire into a setting read at setup connects"),
        );
        sources.push(source);
        readers.push(reader);
    }
    AStreamWithWiresIntoSettings {
        _runner: runner,
        stream,
        sources,
        readers,
        links,
    }
}

/// One wire into a setting, its source writing nothing as it sets up: the stream, the source
/// and the reader.
fn a_running_stream_with_a_wire_into_a_setting(
    project_directory: &std::path::Path,
) -> AStreamWithWiresIntoSettings {
    a_running_stream_with_wires_into_settings(project_directory, 1, |_| {})
}

fn on_the_tests_own_runtime<T>(operation: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
        .block_on(operation)
}

fn start(stream: &Arc<LoadedStreamInThisRuntime>) {
    stream.start().expect("the stream starts");
    stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("every node runs");
}

/// The settings each setup of `reader` read once it has run `setups`, waiting for them.
fn once_it_has_run(reader: Uuid, setups: usize) -> Vec<Map<String, Value>> {
    let deadline = Instant::now() + A_REBUILD_IS_RUN_WITHIN;
    loop {
        let read = the_settings_each_setup_read(reader);
        if read.len() >= setups || Instant::now() >= deadline {
            return read;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The kernel size each setup of `reader` read, once any rebuild a wire asked for has run.
fn the_kernel_sizes_once_settled(reader: Uuid) -> Vec<Value> {
    std::thread::sleep(ANY_REBUILD_HAS_RUN_AFTER);
    the_kernel_sizes(&the_settings_each_setup_read(reader))
}

fn the_kernel_sizes(setups: &[Map<String, Value>]) -> Vec<Value> {
    setups
        .iter()
        .map(|settings| settings.get("kernel_size").cloned().unwrap_or(Value::Null))
        .collect()
}

/// Mental-revert: setting the reader up beside its source, as every node was, builds it with
/// 3 and then rebuilds it with 5.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_node_is_first_built_with_the_value_its_wire_carried_as_its_source_set_up() {
    let project_directory = tempfile::tempdir().unwrap();
    let wired = a_running_stream_with_wires_into_settings(project_directory.path(), 1, |sources| {
        write_from_the_source_at_its_setup(sources[0], 5);
    });

    assert_eq!(
        the_kernel_sizes_once_settled(wired.readers[0]),
        [json!(5)],
        "one setup, with the wire's value"
    );
    wired.stream.stop().expect("the stream stops");
}

/// Mental-revert: refusing the link into the setting fails the connect; reading the wire's
/// values without rebuilding leaves the node built with 3.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn each_value_a_wire_carries_into_a_setting_rebuilds_its_node_once_with_it() {
    let project_directory = tempfile::tempdir().unwrap();
    let wired = a_running_stream_with_a_wire_into_a_setting(project_directory.path());
    let (source, reader) = (wired.sources[0], wired.readers[0]);

    assert_eq!(
        the_kernel_sizes(&once_it_has_run(reader, 1)),
        [json!(3)],
        "a source that wrote nothing as it set up leaves its reader built with its own value"
    );
    write_from_the_source(source, 5).expect("the source writes");
    assert_eq!(
        the_kernel_sizes(&once_it_has_run(reader, 2)),
        [json!(3), json!(5)]
    );
    write_from_the_source(source, 7).expect("the source writes");
    assert_eq!(
        the_kernel_sizes(&once_it_has_run(reader, 3)),
        [json!(3), json!(5), json!(7)]
    );
    write_from_the_source(source, 7).expect("the source writes");
    assert_eq!(
        the_kernel_sizes_once_settled(reader).len(),
        3,
        "a value its node is built with rebuilds nothing"
    );
    wired.stream.stop().expect("the stream stops");
}

/// Two nodes whose settings read at setup are wired from each other both set up, together, and
/// the start that compiles them returns.
///
/// Mental-revert: waiting for each node's sources before letting it set up waits on a node that
/// waits on it in turn, and the start never returns before its bound.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn two_nodes_wired_into_each_others_settings_both_set_up() {
    let project_directory = tempfile::tempdir().unwrap();
    let source_type = the_node_type_whose_output_a_test_writes();
    let runner = Runner::new().expect("Runner::new");
    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory.path(),
            "main",
        ))
        .expect("an empty stream loads");
    let [first, second] = [(); 2].map(|()| {
        stream
            .add_node(NodeSpec::new(source_type.clone(), Value::Null))
            .expect("the source is added")
    });
    for (from, to) in [(&first, &second), (&second, &first)] {
        stream
            .connect(
                OutputLinkPortRef::new(from.clone(), THE_SOURCES_OUTPUT),
                InputLinkPortRef::new(to.clone(), THE_SOURCES_SETTING),
            )
            .expect("a Value wire into a setting read at setup connects");
    }

    let started_at = Instant::now();
    start(&stream);
    assert!(
        started_at.elapsed() < Duration::from_secs(10),
        "the start returned within {:?}",
        started_at.elapsed()
    );
    stream.stop().expect("the stream stops");
}

/// A node whose wired setting's source fails its setup is still built, with the setting's own
/// value, once that source has failed.
///
/// Mental-revert: waiting for the source to set up rather than to fail holds the reader back
/// until the wait's bound, and it is never built in the time the test gives it.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_node_whose_wired_settings_source_fails_its_setup_is_built_with_its_own_value() {
    let project_directory = tempfile::tempdir().unwrap();
    let wired = a_stream_with_wires_into_settings(project_directory.path(), 1);
    fail_the_setup_of_the_source(wired.sources[0]);

    let _ = wired.stream.start();
    assert_eq!(
        the_kernel_sizes(&once_it_has_run(wired.readers[0], 1)),
        [json!(3)]
    );
    let _ = wired.stream.stop();
}

/// When the values of a node set up after the source of its wired setting are not handed to
/// it, that node is never let set up, and the start is refused as a compile is.
///
/// Mental-revert: letting the node set up whatever its delivery answered builds it with the
/// stored 3.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_node_whose_values_are_not_handed_over_after_its_source_set_up_never_sets_up() {
    let project_directory = tempfile::tempdir().unwrap();
    let wired = a_stream_with_wires_into_settings(project_directory.path(), 1);
    write_from_the_source_at_its_setup(wired.sources[0], A_KERNEL_SIZE_IT_IS_NOT_HANDED);

    let refusal = wired
        .stream
        .start()
        .expect_err("the start is refused as its compile is")
        .to_string();
    assert!(refusal.contains("is not taken"), "{refusal}");
    std::thread::sleep(ANY_REBUILD_HAS_RUN_AFTER);
    assert!(
        the_settings_each_setup_read(wired.readers[0]).is_empty(),
        "the reader never set up"
    );
    let _ = wired.stream.stop();
}

/// Mental-revert: with no window for a burst to settle in, the first value rebuilds its node at
/// once and the other node's rebuild runs after, torn down once the first is set up again.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_burst_of_values_on_wires_into_settings_rebuilds_each_node_once_with_its_last() {
    let project_directory = tempfile::tempdir().unwrap();
    let wired = a_running_stream_with_wires_into_settings(project_directory.path(), 2, |_| {});
    let (first, second) = (wired.readers[0], wired.readers[1]);
    once_it_has_run(first, 1);
    once_it_has_run(second, 1);
    let before_the_burst = each_setup_and_teardown_run().len();

    for (source, kernel_size) in [(0, 4), (1, 5), (0, 6), (1, 9)] {
        write_from_the_source(wired.sources[source], kernel_size).expect("the source writes");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(the_kernel_sizes_once_settled(first), [json!(3), json!(6)]);
    assert_eq!(
        the_kernel_sizes(&the_settings_each_setup_read(second)),
        [json!(3), json!(9)]
    );
    let ran: Vec<WhatANodeRan> = each_setup_and_teardown_run()[before_the_burst..]
        .iter()
        .map(|(_, ran)| *ran)
        .collect();
    assert_eq!(
        ran,
        [
            WhatANodeRan::Teardown,
            WhatANodeRan::Teardown,
            WhatANodeRan::Setup,
            WhatANodeRan::Setup
        ],
        "one rebuild of both nodes"
    );
    wired.stream.stop().expect("the stream stops");
}

/// Mental-revert: leaving a node its rebuild refused in its failed state never builds it with
/// 3 again, and retrying the refused value rebuilds it over and over.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_value_on_a_wire_its_node_does_not_build_with_leaves_it_built_with_its_old_one() {
    let project_directory = tempfile::tempdir().unwrap();
    let wired = a_running_stream_with_a_wire_into_a_setting(project_directory.path());
    let (source, reader) = (wired.sources[0], wired.readers[0]);
    once_it_has_run(reader, 1);

    write_from_the_source(source, A_KERNEL_SIZE_IT_CANNOT_BUILD_WITH).expect("the source writes");
    once_it_has_run(reader, 3);
    assert_eq!(
        the_kernel_sizes_once_settled(reader),
        [
            json!(3),
            json!(A_KERNEL_SIZE_IT_CANNOT_BUILD_WITH),
            json!(3)
        ],
        "the refused build, then the old value's"
    );
    wired
        .stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("the node runs with its old value");

    write_from_the_source(source, 5).expect("the source writes");
    assert_eq!(
        the_kernel_sizes(&once_it_has_run(reader, 4)).last(),
        Some(&json!(5)),
        "another value is taken"
    );
    wired.stream.stop().expect("the stream stops");
}

/// Mental-revert: putting back every value of the burst rebuilds the node that built with its
/// value back to 3 too.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_value_one_node_refuses_in_a_burst_puts_back_that_nodes_value_alone() {
    let project_directory = tempfile::tempdir().unwrap();
    let wired = a_running_stream_with_wires_into_settings(project_directory.path(), 2, |_| {});
    let (refusing, building) = (wired.readers[0], wired.readers[1]);
    once_it_has_run(refusing, 1);
    once_it_has_run(building, 1);

    write_from_the_source(wired.sources[0], A_KERNEL_SIZE_IT_CANNOT_BUILD_WITH)
        .expect("the source writes");
    write_from_the_source(wired.sources[1], 5).expect("the source writes");
    once_it_has_run(refusing, 3);

    assert_eq!(
        the_kernel_sizes_once_settled(refusing),
        [
            json!(3),
            json!(A_KERNEL_SIZE_IT_CANNOT_BUILD_WITH),
            json!(3)
        ]
    );
    assert_eq!(
        the_kernel_sizes(&the_settings_each_setup_read(building)),
        [json!(3), json!(5)],
        "the node that built with its value keeps it"
    );
    wired.stream.stop().expect("the stream stops");
}

/// Mental-revert: forgetting the wire's value without a rebuild leaves the node built with 6.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn a_wire_taken_off_a_setting_rebuilds_its_node_with_the_settings_own_value() {
    let project_directory = tempfile::tempdir().unwrap();
    let wired = a_running_stream_with_a_wire_into_a_setting(project_directory.path());
    let (source, reader) = (wired.sources[0], wired.readers[0]);
    once_it_has_run(reader, 1);
    write_from_the_source(source, 6).expect("the source writes");
    once_it_has_run(reader, 2);

    wired
        .stream
        .disconnect(&wired.links[0])
        .expect("the wire is taken off");
    assert_eq!(
        the_kernel_sizes(&once_it_has_run(reader, 3)),
        [json!(3), json!(6), json!(3)]
    );
    assert_eq!(the_kernel_sizes_once_settled(reader).len(), 3);
    wired.stream.stop().expect("the stream stops");
}

/// Mental-revert: building a node at start with its stored value alone builds it with 3, then
/// rebuilds it with 6 once its source writes again.
#[test]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
fn the_value_a_wire_last_carried_into_a_setting_builds_its_node_at_the_next_start() {
    let project_directory = tempfile::tempdir().unwrap();
    let wired = a_running_stream_with_a_wire_into_a_setting(project_directory.path());
    let (source, reader) = (wired.sources[0], wired.readers[0]);
    once_it_has_run(reader, 1);

    write_from_the_source(source, 6).expect("the source writes");
    once_it_has_run(reader, 2);
    wired
        .stream
        .stop_playback_keeping_the_graph()
        .expect("the playback stops, its graph held");
    start(&wired.stream);

    let setups = once_it_has_run(reader, 3);
    assert_eq!(the_kernel_sizes(&setups), [json!(3), json!(6), json!(6)]);
    wired.stream.stop().expect("the stream stops");
}
