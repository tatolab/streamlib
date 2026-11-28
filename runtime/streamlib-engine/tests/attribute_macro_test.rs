// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_engine::core::GeneratedNode;
use streamlib_engine::core::{EmptyConfig, Result, RuntimeContextLimitedAccess};
use streamlib_engine::iceoryx2::{InputMailboxes, OutputWriter};

#[streamlib::sdk::node(
    execution = reactive,
    input("video_in", delivery_profile = "newest", pin_type = Texture),
    output("video_out", pin_type = Texture, formats = ["rgba8_unorm"]),
)]
pub fn test_node(
    inputs: InputMailboxes,
    outputs: OutputWriter,
) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
    move |_ctx| {
        let _ = (&inputs, &outputs);
        Ok(())
    }
}

#[test]
fn the_node_function_is_emitted_beside_a_module_named_after_its_node() {
    fn assert_node_type<T: GeneratedNode>() {}
    assert_node_type::<TestNode::Node>();
}

#[test]
fn test_input_link_module_exists() {
    fn assert_input_marker<T: streamlib_engine::core::InputPortMarker>() {}
    assert_input_marker::<TestNode::InputLink::video_in>();
}

#[test]
fn test_output_link_module_exists() {
    fn assert_output_marker<T: streamlib_engine::core::OutputPortMarker>() {}
    assert_output_marker::<TestNode::OutputLink::video_out>();
}

#[test]
fn test_port_marker_names() {
    use streamlib_engine::core::{InputPortMarker, OutputPortMarker};

    assert_eq!(
        <TestNode::InputLink::video_in as InputPortMarker>::PORT_NAME,
        "video_in"
    );
    assert_eq!(
        <TestNode::OutputLink::video_out as OutputPortMarker>::PORT_NAME,
        "video_out"
    );
}

#[test]
fn test_node_instantiation() {
    let node = TestNode::Node::from_config(EmptyConfig).unwrap();
    assert_eq!(node.name(), "TestNode");
}

#[test]
fn a_node_declaring_no_config_takes_none_and_says_which_key_had_nowhere_to_go() {
    let refusal = serde_json::from_value::<EmptyConfig>(serde_json::json!({ "leftover": 1 }))
        .expect_err("a populated configuration must be refused, not discarded");
    assert!(
        refusal.to_string().contains("leftover"),
        "the refusal must name the key with nowhere to go: {refusal}"
    );

    let from_empty: EmptyConfig = serde_json::from_value(serde_json::json!({})).unwrap();
    assert_eq!(
        serde_json::to_value(from_empty).unwrap(),
        serde_json::json!({})
    );
    serde_json::from_value::<EmptyConfig>(serde_json::Value::Null)
        .expect("a nil configuration is still no configuration");

    let node = TestNode::Node::from_config(EmptyConfig).unwrap();
    assert_eq!(node.name(), "TestNode");
}

#[test]
fn a_node_declaring_no_config_publishes_an_empty_object_schema() {
    let descriptor = TestNode::Node::descriptor().expect("a descriptor");
    let config_schema = descriptor
        .config_schema
        .expect("every macro-emitted descriptor carries a document");
    assert_eq!(config_schema["type"], "object");
    assert_eq!(config_schema["additionalProperties"], false);
    assert!(config_schema.get("properties").is_none());
}

const PROBE_NODE_DESCRIPTION: &str = "Records `video_out` to `path`.";

#[streamlib::sdk::node(
    execution = manual,
    output(
        "video_out",
        pin_type = Texture,
        formats = ["rgba8_unorm"],
        description = "Frames as *recorded*."
    ),
)]
#[meta(category = "Probes/Recording", description = PROBE_NODE_DESCRIPTION)]
pub fn described_probe_node(outputs: OutputWriter) {
    drop(outputs);
}

#[test]
fn the_class_short_name_is_the_node_function_named_in_upper_camel_case() {
    // `NAME` is the display-name default's only carrier. It comes off the function the
    // attribute is attached to — never a string in the attribute, and never recovered by
    // splitting the import path.
    assert_eq!(TestNode::Node::NAME, "TestNode");
    assert_eq!(
        BareNode::Node::NAME,
        "BareNode",
        "a bare `#[node]` still names its class"
    );
}

#[streamlib::sdk::node(execution = manual)]
pub fn bare_node() {}

#[test]
fn a_bare_node_is_named_by_its_import_path() {
    // The class path is the whole identity, and it names the module the macro expanded in —
    // nothing is synthesized and no file is read.
    assert_eq!(
        BareNode::node_class_import_path().as_str(),
        "attribute_macro_test::BareNode"
    );
}

#[test]
fn the_descriptor_carries_the_short_name_and_the_import_path_apart() {
    // Two fields, two jobs: the path is identity, the short name is only the display default.
    // Collapsing either into the other is the regression.
    let descriptor =
        <TestNode::Node as GeneratedNode>::descriptor().expect("the macro emits a descriptor");
    assert_eq!(descriptor.node_class_short_name.as_str(), "TestNode");
    assert_eq!(
        descriptor.node_class_import_path.as_str(),
        "attribute_macro_test::TestNode"
    );
}

#[streamlib::sdk::node(
    execution = reactive,
    input(
        "audio",
        delivery_profile = "ordered",
        audio_window(
            sample_rate = 16_000,
            channels = 1,
            dtype = "f32",
            window_size = 512,
            hop = 160
        ),
        pin_type = Audio,
    ),
    output("windows", pin_type = String, flow = Stream),
)]
pub fn windowed_audio_consumer() -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
    |_ctx| Ok(())
}

#[test]
fn the_descriptor_carries_the_window_contract_its_port_declared() {
    let descriptor = <WindowedAudioConsumer::Node as GeneratedNode>::descriptor()
        .expect("the macro emits a descriptor");
    let audio = descriptor
        .inputs
        .iter()
        .find(|port| port.name == "audio")
        .expect("the audio port is in the descriptor");

    assert_eq!(
        audio.audio_window,
        Some(
            streamlib_engine::core::descriptors::AudioWindowContract::Declaration(
                streamlib_engine::core::descriptors::AudioWindowContractDeclaredValues {
                    sample_rate: 16_000,
                    channels: Some(1),
                    dtype: "f32".to_string(),
                    window_size: 512,
                    hop: 160,
                }
            )
        )
    );
}

// A windowed consumer that names no channel count: its windows carry whatever its source
// sends, which is what lets a graph grow a source without editing every consumer downstream of
// it.
#[streamlib::sdk::node(
    execution = reactive,
    input(
        "audio",
        delivery_profile = "ordered",
        audio_window(sample_rate = 48_000, dtype = "f32", window_size = 960, hop = 960),
        pin_type = Audio,
    ),
)]
pub fn source_following_audio_consumer()
-> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
    |_ctx| Ok(())
}

#[test]
fn a_port_declaring_no_channel_count_carries_the_source_following_contract() {
    let descriptor = <SourceFollowingAudioConsumer::Node as GeneratedNode>::descriptor()
        .expect("the macro emits a descriptor");
    let audio = descriptor
        .inputs
        .iter()
        .find(|port| port.name == "audio")
        .expect("the audio port is in the descriptor");

    assert_eq!(
        audio.audio_window,
        Some(
            streamlib_engine::core::descriptors::AudioWindowContract::Declaration(
                streamlib_engine::core::descriptors::AudioWindowContractDeclaredValues {
                    sample_rate: 48_000,
                    channels: None,
                    dtype: "f32".to_string(),
                    window_size: 960,
                    hop: 960,
                }
            )
        )
    );
}

#[test]
fn a_port_declaring_no_window_contract_carries_none_into_the_descriptor() {
    let descriptor = <WindowedAudioConsumer::Node as GeneratedNode>::descriptor()
        .expect("the macro emits a descriptor");

    assert!(
        descriptor
            .outputs
            .iter()
            .all(|port| port.audio_window.is_none()),
        "an output port declares no contract"
    );
    let unwindowed =
        <TestNode::Node as GeneratedNode>::descriptor().expect("the macro emits a descriptor");
    assert!(
        unwindowed
            .inputs
            .iter()
            .all(|port| port.audio_window.is_none()),
        "a port that declared no contract carries none"
    );
}

#[streamlib::sdk::node(
    execution = manual,
    input("signals", delivery_profile = "ordered", pin_type = Audio),
    output("telemetry", pin_type = Audio, rate = 48000, dtype = "f64"),
)]
pub fn stream_facts_test_node() {}

#[test]
fn a_ports_stream_facts_reach_the_descriptor_the_macro_emits_as_its_pins_annotations() {
    use streamlib_engine::core::descriptors::{
        ChannelsOutputPortSampleRate, ChannelsOutputPortSampleType,
    };

    let descriptor = StreamFactsTestNode::Node::descriptor().expect("a descriptor");
    let telemetry = descriptor.outputs[0]
        .declared_pin()
        .expect("a typed output")
        .annotations;
    assert_eq!(
        telemetry.sample_rate,
        Some(ChannelsOutputPortSampleRate::Stated(48_000))
    );
    assert_eq!(
        telemetry.sample_type,
        Some(ChannelsOutputPortSampleType::F64)
    );
}

#[streamlib::sdk::node(
    execution = reactive,
    input("sample_rate", value = u32, default = 48_000),
    output("telemetry", pin_type = Audio, rate = from_input(sample_rate)),
)]
pub fn rate_following_test_node(
    sample_rate: streamlib_engine::core::prelude::Input<u32>,
) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
    move |_ctx| {
        let _ = sample_rate.get();
        Ok(())
    }
}

#[test]
fn a_rate_read_from_a_value_input_reaches_the_descriptor_naming_that_input() {
    use streamlib_engine::core::descriptors::ChannelsOutputPortSampleRate;

    let descriptor = RateFollowingTestNode::Node::descriptor().expect("a descriptor");
    let telemetry = descriptor.outputs[0]
        .declared_pin()
        .expect("a typed output")
        .annotations;
    assert_eq!(
        telemetry.sample_rate,
        Some(ChannelsOutputPortSampleRate::FromSetting {
            setting: "sample_rate".to_owned()
        })
    );
}

#[test]
fn a_node_and_a_port_description_reach_the_descriptor_as_written_from_a_const_or_a_literal() {
    let descriptor = DescribedProbeNode::Node::descriptor().expect("a descriptor");
    assert_eq!(descriptor.description, PROBE_NODE_DESCRIPTION);
    assert_eq!(descriptor.outputs[0].description, "Frames as *recorded*.");
}

#[test]
fn a_node_category_reaches_the_descriptor_as_the_path_written_and_none_when_undeclared() {
    let described = DescribedProbeNode::Node::descriptor().expect("a descriptor");
    assert_eq!(described.category.as_deref(), Some("Probes/Recording"));
    let undescribed = TestNode::Node::descriptor().expect("a descriptor");
    assert_eq!(undescribed.category, None);
}

#[test]
fn a_node_declaring_no_description_carries_the_empty_string() {
    let descriptor = TestNode::Node::descriptor().expect("a descriptor");
    assert_eq!(descriptor.description, "");
    assert_eq!(descriptor.inputs[0].description, "");
}
