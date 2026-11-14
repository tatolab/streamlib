// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// Every key a rendered port may carry. A port that grows a type field
/// again fails here, whatever the field is named.
const PORT_INFO_KEYS: [&str; 5] = [
    "name",
    "description",
    "port_kind",
    "delivery_profile",
    "family",
];
const PORT_DESCRIPTOR_KEYS: [&str; 5] = [
    "name",
    "description",
    "required",
    "delivery_profile",
    "family",
];
const PORT_INFO_WITH_A_CONTRACT_KEYS: [&str; 6] = [
    "name",
    "description",
    "port_kind",
    "delivery_profile",
    "audio_window",
    "family",
];
const PORT_DESCRIPTOR_WITH_A_CONTRACT_KEYS: [&str; 6] = [
    "name",
    "description",
    "required",
    "delivery_profile",
    "audio_window",
    "family",
];
const FORBIDDEN_PORT_TYPE_KEYS: [&str; 4] = ["data_type", "schema", "type", "schema_ident"];

fn assert_renders_exactly(json: &serde_json::Value, expected: &[&str]) {
    let rendered: Vec<&String> = json.as_object().unwrap().keys().collect();
    assert_eq!(
        rendered.len(),
        expected.len(),
        "a rendered port carries exactly {expected:?}; got {rendered:?}"
    );
    for key in expected {
        assert!(json.get(key).is_some(), "missing `{key}` in {json}");
    }
}

fn assert_carries_no_type_key(json: &serde_json::Value) {
    for key in FORBIDDEN_PORT_TYPE_KEYS {
        assert!(
            json.get(key).is_none(),
            "port rendering must carry no type key; found `{key}` in {json}"
        );
    }
}

/// The contract is opt-in: a port declaring none renders exactly the four
/// keys it always did, with no `audio_window` present as a null.
#[test]
fn port_info_output_renders_exactly_the_declared_keys() {
    let port = crate::core::graph::PortInfo {
        name: "video_in".to_string(),
        description: "Frames to convert".to_string(),
        port_kind: crate::core::graph::PortKind::Data,
        delivery_profile: Some("newest".to_string()),
        borrow_quota: None,
        audio_window: None,
        pin_default: None,
        family: crate::core::descriptors::PortFamilyDeclaration::of_family(
            crate::core::descriptors::PortFamily::Image,
        ),
    };
    let json = serde_json::to_value(PortInfoOutput::from(&port)).unwrap();

    assert_eq!(json["name"], "video_in");
    assert_eq!(json["description"], "Frames to convert");
    assert_eq!(json["port_kind"], "data");
    assert_eq!(json["delivery_profile"], "newest");
    assert_renders_exactly(&json, &PORT_INFO_KEYS);
}

fn a_node_whose_output_is(
    family: crate::core::descriptors::PortFamily,
    config: Option<serde_json::Value>,
    frame_rate: Option<crate::core::descriptors::ImageOutputPortFrameRate>,
) -> crate::core::graph::ProcessorNode {
    crate::core::graph::ProcessorNode::new(
        streamlib_processor_schema::ProcessorClassImportPath::new("tests.CameraSource")
            .expect("a legal import path"),
        Some(crate::core::descriptors::NodeKind::Process),
        "CameraSource",
        config,
        Vec::new(),
        vec![crate::core::graph::PortInfo {
            name: "video".to_string(),
            description: "Frames".to_string(),
            port_kind: crate::core::graph::PortKind::Data,
            delivery_profile: None,
            borrow_quota: None,
            audio_window: None,
            pin_default: None,
            family: crate::core::descriptors::PortFamilyDeclaration {
                frame_rate,
                ..crate::core::descriptors::PortFamilyDeclaration::of_family(family)
            },
        }],
    )
}

fn a_node_of_kind(
    node_kind: Option<crate::core::descriptors::NodeKind>,
) -> crate::core::graph::ProcessorNode {
    crate::core::graph::ProcessorNode::new(
        streamlib_processor_schema::ProcessorClassImportPath::new("tests:Lfo")
            .expect("a legal import path"),
        node_kind,
        "Lfo",
        None,
        Vec::new(),
        Vec::new(),
    )
}

#[test]
fn a_node_renders_its_kind_and_a_function_node_its_language_beside_it() {
    use crate::core::descriptors::{FunctionNodeLanguage, NodeKind};
    let render = |node_kind| {
        serde_json::to_value(ProcessorNodeOutput::from(&a_node_of_kind(node_kind))).unwrap()
    };

    let process = render(Some(NodeKind::Process));
    assert_eq!(process["kind"], "process");
    assert!(process.get("language").is_none(), "{process}");

    let shader = render(Some(NodeKind::Shader));
    assert_eq!(shader["kind"], "shader");
    assert!(shader.get("language").is_none(), "{shader}");

    let function = render(Some(NodeKind::Function {
        language: FunctionNodeLanguage::Python,
    }));
    assert_eq!(function["kind"], "function");
    assert_eq!(function["language"], "python");
}

#[test]
fn a_node_whose_type_nothing_registers_renders_no_kind() {
    let unregistered =
        serde_json::to_value(ProcessorNodeOutput::from(&a_node_of_kind(None))).unwrap();
    assert!(unregistered.get("kind").is_none(), "{unregistered}");
    assert!(unregistered.get("language").is_none(), "{unregistered}");
}

#[test]
fn a_catalog_entry_renders_its_kind_and_a_function_node_its_language_beside_it() {
    use crate::core::descriptors::{FunctionNodeLanguage, NodeKind};
    let mut descriptor = crate::core::ProcessorDescriptor::new(
        crate::core::descriptors::ProcessorClassShortName::new("Lfo").expect("a short name"),
        streamlib_processor_schema::ProcessorClassImportPath::new("tests:Lfo")
            .expect("a legal import path"),
        "oscillates",
    );
    let process = serde_json::to_value(ProcessorDescriptorOutput::from(&descriptor)).unwrap();
    assert_eq!(process["kind"], "process");
    assert!(process.get("language").is_none(), "{process}");

    descriptor.node_kind = NodeKind::Function {
        language: FunctionNodeLanguage::Rust,
    };
    let function = serde_json::to_value(ProcessorDescriptorOutput::from(&descriptor)).unwrap();
    assert_eq!(function["kind"], "function");
    assert_eq!(function["language"], "rust");
}

#[test]
fn an_image_output_renders_the_frame_rate_its_config_declares_as_its_stream_facts() {
    let rendered = serde_json::to_value(ProcessorNodeOutput::from(&a_node_whose_output_is(
        crate::core::descriptors::PortFamily::Image,
        Some(serde_json::json!({"frames_per_second": 30})),
        Some(
            crate::core::descriptors::ImageOutputPortFrameRate::FromConfig {
                config_key: "frames_per_second".into(),
            },
        ),
    )))
    .unwrap();
    let output = &rendered["ports"]["outputs"][0];

    assert_eq!(
        output["stream_facts"],
        serde_json::json!({"family": "image", "frame_rate": {"numerator": 30, "denominator": 1}})
    );
    assert_renders_exactly(
        output,
        &[
            "name",
            "description",
            "port_kind",
            "delivery_profile",
            "stream_facts",
            "family",
        ],
    );
}

#[test]
fn an_output_port_renders_the_facts_it_carries_now() {
    let node = a_node_whose_output_is(crate::core::descriptors::PortFamily::Packets, None, None);
    let unstated = serde_json::to_value(ProcessorNodeOutput::from(&node)).unwrap();
    assert_renders_exactly(&unstated["ports"]["outputs"][0], &PORT_INFO_KEYS);

    node.stream_facts_of_output_ports()
        .replace_facts_of(
            "video",
            crate::core::descriptors::PacketsStreamFacts {
                rfc6381_codec: Some("avc1.64001f".to_string()),
                coded_width: Some(1280),
                coded_height: Some(720),
                ..Default::default()
            }
            .into(),
        )
        .expect("a Packets port carries Packets facts");
    let replaced = serde_json::to_value(ProcessorNodeOutput::from(&node)).unwrap();
    assert_eq!(
        replaced["ports"]["outputs"][0]["stream_facts"],
        serde_json::json!({
            "family": "packets",
            "rfc6381_codec": "avc1.64001f",
            "coded_width": 1280,
            "coded_height": 720,
        })
    );
}

#[test]
fn a_records_port_renders_its_values_unchecked_in_the_graph_and_the_registry() {
    let node = a_node_whose_output_is(crate::core::descriptors::PortFamily::Records, None, None);
    let rendered = serde_json::to_value(ProcessorNodeOutput::from(&node)).unwrap();
    assert_eq!(
        rendered["ports"]["outputs"][0]["values_checked"],
        serde_json::json!(false)
    );

    let declared = crate::core::PortDescriptor::new(
        "detections",
        "Detections",
        true,
        crate::core::descriptors::PortFamilyDeclaration::of_family(
            crate::core::descriptors::PortFamily::Records,
        ),
    );
    let json = serde_json::to_value(PortDescriptorOutput::from(&declared)).unwrap();
    assert_eq!(json["values_checked"], serde_json::json!(false));
    assert_renders_exactly(
        &json,
        &[
            "name",
            "description",
            "required",
            "delivery_profile",
            "values_checked",
            "family",
        ],
    );
}

/// The type layer is gone, not omitted-when-empty: no spelling of a port
/// type may appear on the wire, even as an absent optional field.
#[test]
fn port_info_output_carries_no_type_key_under_any_spelling() {
    let port = crate::core::graph::PortInfo {
        name: "data".to_string(),
        description: String::new(),
        port_kind: crate::core::graph::PortKind::Data,
        delivery_profile: None,
        borrow_quota: None,
        audio_window: None,
        pin_default: None,
        family: crate::core::descriptors::PortFamilyDeclaration::of_family(
            crate::core::descriptors::PortFamily::Image,
        ),
    };
    let json = serde_json::to_value(PortInfoOutput::from(&port)).unwrap();
    assert_carries_no_type_key(&json);
}

#[test]
fn port_descriptor_output_carries_no_type_key() {
    let pd = crate::core::PortDescriptor::new(
        "video",
        "Video output",
        true,
        crate::core::descriptors::PortFamilyDeclaration::of_family(
            crate::core::descriptors::PortFamily::Image,
        ),
    )
    .with_delivery_profile("ordered");
    let json = serde_json::to_value(PortDescriptorOutput::from(&pd)).unwrap();

    assert_eq!(json["name"], "video");
    assert_eq!(json["description"], "Video output");
    assert_eq!(json["delivery_profile"], "ordered");
    assert_renders_exactly(&json, &PORT_DESCRIPTOR_KEYS);
    assert_carries_no_type_key(&json);
}

fn declared_window_contract() -> crate::core::descriptors::AudioWindowContract {
    crate::core::descriptors::AudioWindowContract::Declaration(
        crate::core::descriptors::AudioWindowContractDeclaredValues {
            sample_rate: 16_000,
            channels: Some(1),
            dtype: "f32".to_string(),
            window_size: 512,
            hop: 512,
        },
    )
}

#[test]
fn a_contract_bearing_port_renders_its_contract_beside_the_four() {
    let port = crate::core::graph::PortInfo {
        name: "audio".to_string(),
        description: "Samples to frame".to_string(),
        port_kind: crate::core::graph::PortKind::Data,
        delivery_profile: Some("ordered".to_string()),
        borrow_quota: None,
        audio_window: Some(declared_window_contract()),
        pin_default: None,
        family: crate::core::descriptors::PortFamilyDeclaration::of_family(
            crate::core::descriptors::PortFamily::Image,
        ),
    };
    let json = serde_json::to_value(PortInfoOutput::from(&port)).unwrap();

    assert_eq!(
        json["audio_window"],
        serde_json::json!({
            "resolved_from": "declaration",
            "sample_rate": 16_000,
            "channels": 1,
            "dtype": "f32",
            "window_size": 512,
            "hop": 512,
        })
    );
    assert_renders_exactly(&json, &PORT_INFO_WITH_A_CONTRACT_KEYS);
    assert_carries_no_type_key(&json);
}

/// A count the port left to its source is spelled in the rendering rather
/// than left out of it: a reader learns the count follows the source,
/// where a missing key would tell it nothing.
#[test]
fn a_port_that_declared_no_channel_count_renders_it_as_the_source() {
    let port = crate::core::graph::PortInfo {
        name: "audio".to_string(),
        description: String::new(),
        port_kind: crate::core::graph::PortKind::Data,
        delivery_profile: Some("ordered".to_string()),
        borrow_quota: None,
        audio_window: Some(crate::core::descriptors::AudioWindowContract::Declaration(
            crate::core::descriptors::AudioWindowContractDeclaredValues {
                sample_rate: 48_000,
                channels: None,
                dtype: "f32".to_string(),
                window_size: 960,
                hop: 960,
            },
        )),
        pin_default: None,
        family: crate::core::descriptors::PortFamilyDeclaration::of_family(
            crate::core::descriptors::PortFamily::Image,
        ),
    };
    let json = serde_json::to_value(PortInfoOutput::from(&port)).unwrap();

    assert_eq!(
        json["audio_window"],
        serde_json::json!({
            "resolved_from": "declaration",
            "sample_rate": 48_000,
            "channels": "source",
            "dtype": "f32",
            "window_size": 960,
            "hop": 960,
        })
    );
    assert_renders_exactly(&json, &PORT_INFO_WITH_A_CONTRACT_KEYS);
}

#[test]
fn a_port_declaring_the_sentinel_renders_it_as_a_whole_contract() {
    let port = crate::core::graph::PortInfo {
        name: "audio".to_string(),
        description: String::new(),
        port_kind: crate::core::graph::PortKind::Data,
        delivery_profile: Some("ordered".to_string()),
        borrow_quota: None,
        audio_window: Some(crate::core::descriptors::AudioWindowContract::MatchDevice {}),
        pin_default: None,
        family: crate::core::descriptors::PortFamilyDeclaration::of_family(
            crate::core::descriptors::PortFamily::Image,
        ),
    };
    let json = serde_json::to_value(PortInfoOutput::from(&port)).unwrap();

    assert_eq!(
        json["audio_window"],
        serde_json::json!({ "resolved_from": "match_device" })
    );
}

/// A node whose input port declares the sentinel, with `settled` optionally
/// standing in for what its own device gave it.
fn a_node_declaring_the_sentinel(
    settled: Option<crate::iceoryx2::ResolvedAudioWindowContract>,
) -> crate::core::graph::ProcessorNode {
    use crate::core::graph::GraphNodeWithComponents;

    let mut node = crate::core::graph::ProcessorNode::new(
        streamlib_processor_schema::ProcessorClassImportPath::new("tests.SpeakerSink")
            .expect("a legal import path"),
        Some(crate::core::descriptors::NodeKind::Process),
        "SpeakerSink",
        None,
        vec![crate::core::graph::PortInfo {
            name: "audio".to_string(),
            description: "Blocks to play".to_string(),
            port_kind: crate::core::graph::PortKind::Data,
            delivery_profile: Some("ordered".to_string()),
            borrow_quota: None,
            audio_window: Some(crate::core::descriptors::AudioWindowContract::MatchDevice {}),
            pin_default: None,
            family: crate::core::descriptors::PortFamilyDeclaration::of_family(
                crate::core::descriptors::PortFamily::Image,
            ),
        }],
        Vec::new(),
    );

    let contracts = std::sync::Arc::new(
        crate::iceoryx2::DeviceMatchedAudioWindowContractsByInputPort::default(),
    );
    if let Some(contract) = settled {
        contracts.settle_for_input_port("audio", contract);
    }
    node.insert_component_without_rendering_it(
        crate::core::graph::DeviceMatchedAudioWindowContractsComponent(contracts),
    );
    node
}

fn a_playback_stream_of(
    sample_rate: u32,
    channels: u32,
) -> crate::core::context::AudioStreamFormat {
    crate::core::context::AudioStreamFormat {
        sample_rate,
        channels,
        sample_format: crate::core::context::AudioSampleFormat::F32,
    }
}

/// `graph` renders what the device gave rather than the sentinel that asked
/// for it — machine-dependent because the device format is, which is truer
/// than a static lie.
#[test]
fn a_settled_match_device_port_renders_the_five_values_its_device_gave() {
    let settled = crate::iceoryx2::ResolvedAudioWindowContract::from_a_device_stream_format(
        &crate::iceoryx2::AudioWindowContractMatchingADeviceStream {
            device_stream_format: a_playback_stream_of(44_100, 2),
            window_size_in_per_channel_samples: 441,
            hop_in_per_channel_samples: 441,
        },
    )
    .expect("a device format settles a contract");

    let rendered = serde_json::to_value(ProcessorNodeOutput::from(&a_node_declaring_the_sentinel(
        Some(settled),
    )))
    .unwrap();

    assert_eq!(
        rendered["ports"]["inputs"][0]["audio_window"],
        serde_json::json!({
            "resolved_from": "device",
            "sample_rate": 44_100,
            "channels": 2,
            "dtype": "f32",
            "window_size": 441,
            "hop": 441,
        })
    );
}

/// Before its processor's `setup()` opens a device there is nothing to
/// render but the declaration, and rendering a guess in its place would be
/// the static lie the resolved rendering exists instead of.
#[test]
fn an_unsettled_match_device_port_still_renders_the_sentinel() {
    let rendered = serde_json::to_value(ProcessorNodeOutput::from(&a_node_declaring_the_sentinel(
        None,
    )))
    .unwrap();

    assert_eq!(
        rendered["ports"]["inputs"][0]["audio_window"],
        serde_json::json!({ "resolved_from": "match_device" })
    );
}

/// The settled contracts reach a reader on the port that settled them and
/// nowhere else: a second rendering under `components` would be one more
/// copy of the same fact to keep in agreement.
#[test]
fn the_settled_contracts_render_on_the_port_and_not_as_a_component_of_their_own() {
    let rendered = serde_json::to_value(ProcessorNodeOutput::from(&a_node_declaring_the_sentinel(
        None,
    )))
    .unwrap();

    assert!(
        rendered["components"]
            .get("device_matched_audio_window_contracts")
            .is_none(),
        "the settled contracts carry no `components` key of their own; got {}",
        rendered["components"]
    );
}

/// The contract rides `PortDescriptor` into `PortInfo` untouched — the
/// carrier the macro and the wheel's declaration bridge both fill.
#[test]
fn a_declared_contract_survives_the_descriptor_to_port_info_hop() {
    let descriptor = crate::core::PortDescriptor::new(
        "audio",
        "Samples to frame",
        true,
        crate::core::descriptors::PortFamilyDeclaration::of_family(
            crate::core::descriptors::PortFamily::Image,
        ),
    )
    .with_delivery_profile("ordered")
    .with_audio_window_contract(declared_window_contract());

    let port = crate::core::graph::PortInfo::from(&descriptor);

    assert_eq!(port.audio_window, Some(declared_window_contract()));
}

#[test]
fn a_contract_bearing_descriptor_renders_its_contract_too() {
    let descriptor = crate::core::PortDescriptor::new(
        "audio",
        "Samples to frame",
        true,
        crate::core::descriptors::PortFamilyDeclaration::of_family(
            crate::core::descriptors::PortFamily::Image,
        ),
    )
    .with_delivery_profile("ordered")
    .with_audio_window_contract(crate::core::descriptors::AudioWindowContract::MatchDevice {});
    let json = serde_json::to_value(PortDescriptorOutput::from(&descriptor)).unwrap();

    assert_eq!(
        json["audio_window"],
        serde_json::json!({ "resolved_from": "match_device" })
    );
    assert_renders_exactly(&json, &PORT_DESCRIPTOR_WITH_A_CONTRACT_KEYS);
}
