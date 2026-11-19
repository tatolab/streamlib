// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A setting a node reads only when it is set up, set while the stream runs: the node is stopped
//! and built again from the value, once however many of its settings one edit sets, while the
//! stream keeps running; a setup that refuses the value leaves the node in `Error` with its own
//! reason; a paused stream builds it at resume, a stopped one at its start.

use std::sync::Once;

use serde_json::json;
use streamlib_project_repository::StreamGraphOperation;
use uuid::Uuid;

use super::*;
use crate::core::descriptors::{
    NodeInputMode, PinDeclaration, PinType, PortDescriptor, PortFamily, PortFamilyDeclaration,
    SettingReadTime,
};
use crate::core::graph::{GraphNodeWithComponents, NodeRunId, StateComponent};
use crate::core::nodes::{GeneratedNode, NODE_REGISTRY, NodeSpec, NodeState};
use crate::core::runtime::RuntimeOperations;

/// The settings of a [`NodeReadingItsSettingsAtSetup`], each read when it is built.
const SETTINGS_READ_AT_SETUP: [&str; 3] = ["gain", "offset", "scale"];

/// The settings each [`NodeReadingItsSettingsAtSetup`] was built from, in the order it was
/// built, by setting.
static EACH_BUILD_OF_THE_NODE: Mutex<Vec<serde_json::Map<String, serde_json::Value>>> =
    Mutex::new(Vec::new());

/// A node whose settings are each read at setup, as its constructor reads them, and whose setup
/// refuses a negative gain.
#[crate::node(execution = manual)]
pub(crate) struct NodeReadingItsSettingsAtSetup;

impl crate::core::ManualNode for NodeReadingItsSettingsAtSetup::Node {
    fn setup(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        let gain = EACH_BUILD_OF_THE_NODE
            .lock()
            .last()
            .and_then(|settings| settings.get("gain"))
            .and_then(serde_json::Value::as_f64)
            .unwrap_or_default();
        if gain < 0.0 {
            return Err(Error::Configuration(format!(
                "a gain of {gain} would invert the signal; the gain must be 0 or more"
            )));
        }
        Ok(())
    }

    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Register [`NodeReadingItsSettingsAtSetup`] with a Float input per setting, each read at
/// setup, and a constructor recording the settings it builds each node from.
fn register_the_node_reading_its_settings_at_setup() {
    static REGISTERED_ONCE_PER_PROCESS: Once = Once::new();
    REGISTERED_ONCE_PER_PROCESS.call_once(|| {
        let mut descriptor = <NodeReadingItsSettingsAtSetup::Node as GeneratedNode>::descriptor()
            .expect("a #[node] type has a descriptor");
        descriptor.inputs = SETTINGS_READ_AT_SETUP
            .iter()
            .map(|setting| {
                PortDescriptor::new(
                    *setting,
                    *setting,
                    false,
                    PortFamilyDeclaration::of_family(PortFamily::Channels),
                )
                .with_pin_declaration(PinDeclaration::exactly(PinType::Float { exact_size: None }))
                .with_input_mode(NodeInputMode::SettingOnly {
                    read: SettingReadTime::AtSetup,
                })
            })
            .collect();
        NODE_REGISTRY
            .register_with_a_constructor_for_a_test(
                descriptor,
                Box::new(|node| {
                    EACH_BUILD_OF_THE_NODE.lock().push(node.pins.clone());
                    Ok(Box::new(
                        <NodeReadingItsSettingsAtSetup::Node as GeneratedNode>::from_config(
                            Default::default(),
                        )?,
                    ))
                }),
            )
            .expect("the node type registers");
    });
}

/// A stream holding one [`NodeReadingItsSettingsAtSetup`] built with a gain of 1, not started,
/// with the UUID and the id of that node.
fn a_stream_holding_a_node_reading_its_settings_at_setup(
    runner: &Runner,
    project_directory: &std::path::Path,
) -> (Arc<LoadedStreamInThisRuntime>, Uuid, NodeRunId) {
    register_the_node_reading_its_settings_at_setup();
    EACH_BUILD_OF_THE_NODE.lock().clear();
    let stream = an_empty_stream_loaded_into(runner, project_directory, "settings");
    let node_uuid = Uuid::new_v4();
    let mut pins = serde_json::Map::new();
    pins.insert("gain".to_owned(), json!(1.0));
    let node_run_id = stream
        .add_node(
            NodeSpec::new(
                NodeReadingItsSettingsAtSetup::node_class_import_path(),
                json!({}),
            )
            .with_node_uuid(node_uuid)
            .with_pins_and_studio_fields(pins, serde_json::Map::new()),
        )
        .expect("the node is added");
    (stream, node_uuid, node_run_id)
}

fn a_runtime_to_apply_edits_on() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
}

/// Set `setting` of the node `node` on `stream` to `to`, as an edit applied live does.
fn set_the_setting(
    tokio_runtime: &tokio::runtime::Runtime,
    stream: &LoadedStreamInThisRuntime,
    node: Uuid,
    setting: &str,
    to: f64,
) {
    tokio_runtime
        .block_on(stream.apply_a_stream_graph_operation_live(
            StreamGraphOperation::SetNodeInputValue {
                node,
                input: setting.to_owned(),
                from: None,
                to: Some(json!(to)),
            },
        ))
        .expect("the setting is set");
}

fn the_state_of_the_node(stream: &LoadedStreamInThisRuntime, node_run_id: &NodeRunId) -> NodeState {
    stream.compiler.scope(|graph, _tx| {
        graph
            .traversal()
            .v(node_run_id)
            .first()
            .and_then(|node| node.get::<StateComponent>())
            .map(StateComponent::current)
            .expect("the node holds a state")
    })
}

fn the_settings_of_each_build() -> Vec<serde_json::Map<String, serde_json::Value>> {
    EACH_BUILD_OF_THE_NODE.lock().clone()
}

fn the_gain_of_each_build() -> Vec<Option<f64>> {
    the_settings_of_each_build()
        .iter()
        .map(|settings| settings.get("gain").and_then(serde_json::Value::as_f64))
        .collect()
}

/// A gain set on a running node is read by the node built again from it: the build answers
/// the node, the node runs, and the stream never stopped.
///
/// Mental-revert: a setting read at setup set on its pin alone, as a setting read each run is,
/// leaves the node as it was built, so it is built once, with a gain of 1, and answers none.
#[test]
#[serial]
fn a_setting_read_at_setup_set_while_running_builds_its_node_again() {
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let (stream, node, node_run_id) =
        a_stream_holding_a_node_reading_its_settings_at_setup(&runner, project_directory.path());
    stream.start().expect("the stream starts");
    stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("the node runs");
    let tokio_runtime = a_runtime_to_apply_edits_on();

    set_the_setting(&tokio_runtime, &stream, node, "gain", 2.0);
    let built_again = tokio_runtime
        .block_on(stream.build_again_each_node_a_setting_was_set_on())
        .expect("the node is built again");

    assert_eq!(built_again, vec![node]);
    assert_eq!(the_gain_of_each_build(), vec![Some(1.0), Some(2.0)]);
    assert_eq!(
        the_state_of_the_node(&stream, &node_run_id),
        NodeState::Running
    );
    assert_eq!(*stream.status.lock(), RuntimeStatus::Started);
    assert!(
        tokio_runtime
            .block_on(stream.build_again_each_node_a_setting_was_set_on())
            .expect("nothing is left to build")
            .is_empty(),
        "a node built again is built again for the next setting set, not for this one"
    );
    stream.stop().expect("the stream stops");
}

/// Three settings set on one running node before it is built again build it once, from all
/// three.
///
/// Mental-revert: a build logged for each setting set, rather than once a node, builds the node
/// three more times, the first two from a part of the settings set.
#[test]
#[serial]
fn three_settings_set_before_the_build_build_the_node_again_once() {
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let (stream, node, _node_run_id) =
        a_stream_holding_a_node_reading_its_settings_at_setup(&runner, project_directory.path());
    stream.start().expect("the stream starts");
    stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("the node runs");
    let tokio_runtime = a_runtime_to_apply_edits_on();

    for (setting, to) in [("gain", 3.0), ("offset", 0.5), ("scale", 4.0)] {
        set_the_setting(&tokio_runtime, &stream, node, setting, to);
    }
    let built_again = tokio_runtime
        .block_on(stream.build_again_each_node_a_setting_was_set_on())
        .expect("the node is built again");

    assert_eq!(built_again, vec![node]);
    let each_build = the_settings_of_each_build();
    assert_eq!(each_build.len(), 2, "{each_build:?}");
    assert_eq!(
        serde_json::Value::Object(each_build[1].clone()),
        json!({"gain": 3.0, "offset": 0.5, "scale": 4.0})
    );
    stream.stop().expect("the stream stops");
}

/// A gain the node's setup refuses leaves the node in `Error` with the node's own reason, and
/// the gain set back builds it again running.
///
/// Mental-revert: a build answered once its node is built, before its setup, answers a node
/// that then fails, and the refusal carries no reason.
#[test]
#[serial]
fn a_setting_its_setup_refuses_is_refused_with_the_nodes_reason() {
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let (stream, node, node_run_id) =
        a_stream_holding_a_node_reading_its_settings_at_setup(&runner, project_directory.path());
    stream.start().expect("the stream starts");
    stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("the node runs");
    let tokio_runtime = a_runtime_to_apply_edits_on();

    set_the_setting(&tokio_runtime, &stream, node, "gain", -1.0);
    let refusal = tokio_runtime
        .block_on(stream.build_again_each_node_a_setting_was_set_on())
        .expect_err("a gain the setup refuses is refused")
        .to_string();

    assert!(
        refusal.contains("a gain of -1 would invert the signal; the gain must be 0 or more"),
        "{refusal}"
    );
    assert_eq!(
        the_state_of_the_node(&stream, &node_run_id),
        NodeState::Error
    );

    set_the_setting(&tokio_runtime, &stream, node, "gain", 1.0);
    let built_again = tokio_runtime
        .block_on(stream.build_again_each_node_a_setting_was_set_on())
        .expect("the node is built again with its former gain");

    assert_eq!(built_again, vec![node]);
    assert_eq!(
        the_gain_of_each_build(),
        vec![Some(1.0), Some(-1.0), Some(1.0)]
    );
    assert_eq!(
        the_state_of_the_node(&stream, &node_run_id),
        NodeState::Running
    );
    stream.stop().expect("the stream stops");
}

/// A gain set while the stream is paused is built at the resume, and one set before the start
/// at the start: neither is answered as built when it is set.
///
/// Mental-revert: a build logged only for a stream running is lost on a paused one, which then
/// resumes with the node built from a gain of 2.
#[test]
#[serial]
fn a_setting_set_while_paused_or_stopped_is_built_at_resume_or_start() {
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let (stream, node, node_run_id) =
        a_stream_holding_a_node_reading_its_settings_at_setup(&runner, project_directory.path());
    let tokio_runtime = a_runtime_to_apply_edits_on();

    set_the_setting(&tokio_runtime, &stream, node, "gain", 2.0);
    assert!(
        tokio_runtime
            .block_on(stream.build_again_each_node_a_setting_was_set_on())
            .expect("a stream not started builds nothing yet")
            .is_empty()
    );
    stream.start().expect("the stream starts");
    stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("the node runs");
    assert_eq!(the_gain_of_each_build(), vec![Some(2.0)]);

    stream.pause().expect("the stream pauses");
    set_the_setting(&tokio_runtime, &stream, node, "gain", 3.0);
    assert!(
        tokio_runtime
            .block_on(stream.build_again_each_node_a_setting_was_set_on())
            .expect("a paused stream builds nothing yet")
            .is_empty()
    );
    stream.resume().expect("the stream resumes");
    stream
        .wait_until_every_node_is_running(Duration::from_secs(30))
        .expect("the node runs");

    assert_eq!(the_gain_of_each_build(), vec![Some(2.0), Some(3.0)]);
    assert_eq!(
        the_state_of_the_node(&stream, &node_run_id),
        NodeState::Running
    );
    stream.stop().expect("the stream stops");
}
