// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A setting a node reads only when it is set up, set on a playing stream: the edit builds the
//! node again from it and answers the node under `nodes_rebuilt`, once however many of its
//! settings the edit sets; a value the node's setup refuses refuses the edit in the node's own
//! words, and the node is built again from the value it held.

use super::*;

use super::input_value_set_tests::the_input_values_of;

use parking_lot::Mutex;
use serde_json::Map;
use serial_test::serial;
use streamlib::sdk::descriptors::{
    NodeInputMode, PinDeclaration, PinType, PortDescriptor, PortFamily, PortFamilyDeclaration,
    SettingReadTime,
};
use streamlib::sdk::nodes::{DynGeneratedNode, GeneratedNode};

/// The settings of a [`SettingReadAtSetupTestNode`], each read when it is built.
const SETTINGS_READ_AT_SETUP: [&str; 3] = ["gain", "offset", "scale"];

const NODE_READING_ITS_SETTINGS_AT_SETUP: &str = "7c7c7c7c-7c7c-4c7c-8c7c-7c7c7c7c7c7c";

/// The settings each [`SettingReadAtSetupTestNode`] was built from, in the order it was built;
/// the tests reading it run one at a time.
static EACH_BUILD_OF_THE_NODE: Mutex<Vec<Map<String, Value>>> = Mutex::new(Vec::new());

/// A node whose settings are each read at setup, as its constructor reads them, and whose setup
/// refuses a negative gain.
#[streamlib::sdk::node(execution = manual)]
pub(super) struct SettingReadAtSetupTestNode;

impl ManualNode for SettingReadAtSetupTestNode::Node {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let gain = EACH_BUILD_OF_THE_NODE
            .lock()
            .last()
            .and_then(|settings| settings.get("gain"))
            .and_then(Value::as_f64)
            .unwrap_or_default();
        if gain < 0.0 {
            return Err(streamlib::sdk::error::Error::Configuration(format!(
                "a gain of {gain} would invert the signal; the gain must be 0 or more"
            )));
        }
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

/// Register [`SettingReadAtSetupTestNode`] with a Float input per setting, each read at setup,
/// and a constructor recording the settings it builds each node from, once per test binary.
fn register_the_setting_read_at_setup_test_node_once() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        let mut descriptor = <SettingReadAtSetupTestNode::Node as GeneratedNode>::descriptor()
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
                Box::new(|node| -> Result<Box<dyn DynGeneratedNode + Send>> {
                    EACH_BUILD_OF_THE_NODE.lock().push(node.pins.clone());
                    Ok(Box::new(
                        <SettingReadAtSetupTestNode::Node as GeneratedNode>::from_config(
                            Default::default(),
                        )?,
                    ))
                }),
            )
            .expect("the node type registers");
    });
}

/// A project whose stream `main` holds a [`SettingReadAtSetupTestNode`] `amplifier` with a gain
/// of 1, by [`NODE_READING_ITS_SETTINGS_AT_SETUP`].
fn a_project_whose_main_stream_holds_an_amplifier() -> ProjectWithGraphDocuments {
    register_the_setting_read_at_setup_test_node_once();
    EACH_BUILD_OF_THE_NODE.lock().clear();
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [{
            "id": NODE_READING_ITS_SETTINGS_AT_SETUP,
            "name": "amplifier",
            "type": SettingReadAtSetupTestNode::node_class_import_path().as_str(),
            "config": {},
            "input_values": { "gain": 1.0 },
        }],
        "links": [],
    }))
}

fn the_gain_of_each_build() -> Vec<Option<f64>> {
    EACH_BUILD_OF_THE_NODE
        .lock()
        .iter()
        .map(|settings| settings.get("gain").and_then(Value::as_f64))
        .collect()
}

/// A gain committed on a playing stream builds the amplifier again from it, answered under
/// `nodes_rebuilt`; three settings set in one transaction build it once more, from all three.
///
/// Mental-revert: an answer that lists no node built again, as before, leaves `nodes_rebuilt`
/// out; a build for each setting set builds the amplifier three more times.
#[tokio::test(flavor = "multi_thread")]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
async fn a_setting_read_at_setup_builds_its_node_again_and_is_answered_once() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_an_amplifier();
    let (stream_id, _served) = engine
        .the_stream_main_of_the_project_started_and_served(&project)
        .await;
    let stream_asset = project.stream_asset_of("main");
    assert_eq!(the_gain_of_each_build(), vec![Some(1.0)]);

    let applied = engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": stream_asset, "node": NODE_READING_ITS_SETTINGS_AT_SETUP,
                    "input": "gain", "value": 2.0, "author": an_agent_named("alice") }),
        )
        .await
        .expect("the gain is set");

    let the_amplifier_rebuilt =
        json!([{ "stream": stream_id, "node": NODE_READING_ITS_SETTINGS_AT_SETUP }]);
    assert_eq!(applied["nodes_rebuilt"], the_amplifier_rebuilt, "{applied}");
    assert_eq!(the_gain_of_each_build(), vec![Some(1.0), Some(2.0)]);

    let set = |input: &str, value: f64| {
        json!({ "operation": "set_node_input_value", "stream": stream_asset,
                "node": NODE_READING_ITS_SETTINGS_AT_SETUP, "input": input, "value": value })
    };
    let applied = engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({ "operations": [set("gain", 3.0), set("offset", 0.5), set("scale", 4.0)],
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("the three settings are set");

    assert_eq!(applied["nodes_rebuilt"], the_amplifier_rebuilt, "{applied}");
    let each_build = EACH_BUILD_OF_THE_NODE.lock().clone();
    assert_eq!(each_build.len(), 3, "{each_build:?}");
    assert_eq!(
        Value::Object(each_build[2].clone()),
        json!({ "gain": 3.0, "offset": 0.5, "scale": 4.0 })
    );
}

/// A gain the amplifier's setup refuses refuses the edit in the amplifier's own words: the
/// document keeps the gain it held, and the amplifier is built again from it.
///
/// Mental-revert: an edit answered before the node built again finishes its setup is applied,
/// the document saving a gain of -1 and the amplifier left failed.
#[tokio::test(flavor = "multi_thread")]
#[serial]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
async fn a_setting_the_nodes_setup_refuses_is_refused_and_its_former_value_built_again() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_an_amplifier();
    let (_stream_id, _served) = engine
        .the_stream_main_of_the_project_started_and_served(&project)
        .await;
    let stream_asset = project.stream_asset_of("main");

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": stream_asset, "node": NODE_READING_ITS_SETTINGS_AT_SETUP,
                    "input": "gain", "value": -1.0, "author": an_agent_named("alice") }),
        )
        .await
        .expect_err("a gain the setup refuses is refused");

    assert!(
        refusal
            .runtime_words
            .contains("a gain of -1 would invert the signal; the gain must be 0 or more"),
        "{refusal:?}"
    );
    assert_eq!(
        the_input_values_of(
            &project.the_graph_document_of("main"),
            NODE_READING_ITS_SETTINGS_AT_SETUP
        ),
        json!({ "gain": 1.0 })
    );
    assert_eq!(
        the_gain_of_each_build(),
        vec![Some(1.0), Some(-1.0), Some(1.0)]
    );
}
