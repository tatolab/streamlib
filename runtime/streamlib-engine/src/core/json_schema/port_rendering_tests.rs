// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// Every key a rendered port with no exact pin type carries. A port that
/// grows another field fails here, whatever the field is named.
const PORT_INFO_KEYS: [&str; 5] = [
    "name",
    "description",
    "port_kind",
    "delivery_profile",
    "family",
];
/// A picture port is a Texture, so it serves its pin type and flow beside its family.
const PORT_DESCRIPTOR_OF_A_PICTURE_KEYS: [&str; 7] = [
    "name",
    "description",
    "required",
    "delivery_profile",
    "family",
    "pin_type",
    "flow",
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
        delivery_profile: Some("newest".to_string()),
        borrow_quota: None,
        audio_window: None,
        pin_default: None,
        pin: None,
        declared_pin_type: None,
        lifetime: crate::core::descriptors::NodeValueLifetime::Kept,
        former_names: Vec::new(),
        may_be_unset: false,
        family: crate::core::descriptors::PortFamilyDeclaration::of_family(
            crate::core::descriptors::PortFamily::Image,
        ),
        input_mode: None,
        annotations: Default::default(),
    };
    let json = serde_json::to_value(GraphNodePortOnTheWire::from(&port)).unwrap();

    assert_eq!(json["name"], "video_in");
    assert_eq!(json["description"], "Frames to convert");
    assert_eq!(json["port_kind"], "data");
    assert_eq!(json["delivery_profile"], "newest");
    assert_renders_exactly(&json, &PORT_INFO_KEYS);
}

fn a_node_whose_output_is(
    family: crate::core::descriptors::PortFamily,
    input_values: Option<serde_json::Value>,
    frame_rate: Option<crate::core::descriptors::ImageOutputPortFrameRate>,
) -> crate::core::graph::GraphNode {
    crate::core::graph::GraphNode::new(
        streamlib_node_schema::NodeClassImportPath::new("tests.CameraSource")
            .expect("a legal import path"),
        Some(crate::core::descriptors::NodeKind::Process),
        "CameraSource",
        input_values
            .and_then(|input_values| input_values.as_object().cloned())
            .unwrap_or_default(),
        Vec::new(),
        vec![crate::core::graph::PortInfo {
            name: "video".to_string(),
            description: "Frames".to_string(),
            delivery_profile: None,
            borrow_quota: None,
            audio_window: None,
            pin_default: None,
            pin: None,
            declared_pin_type: None,
            lifetime: crate::core::descriptors::NodeValueLifetime::Kept,
            former_names: Vec::new(),
            may_be_unset: false,
            family: crate::core::descriptors::PortFamilyDeclaration {
                frame_rate,
                ..crate::core::descriptors::PortFamilyDeclaration::of_family(family)
            },
            input_mode: None,
            annotations: Default::default(),
        }],
    )
}

fn a_node_of_kind(
    node_kind: Option<crate::core::descriptors::NodeKind>,
) -> crate::core::graph::GraphNode {
    crate::core::graph::GraphNode::new(
        streamlib_node_schema::NodeClassImportPath::new("tests:Lfo").expect("a legal import path"),
        node_kind,
        "Lfo",
        serde_json::Map::new(),
        Vec::new(),
        Vec::new(),
    )
}

#[test]
fn a_node_renders_its_kind_and_a_function_node_its_language_beside_it() {
    use crate::core::descriptors::{FunctionNodeLanguage, NodeKind};
    let render = |node_kind| {
        serde_json::to_value(GraphNodeOutput::from(&a_node_of_kind(node_kind))).unwrap()
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

/// A node of `node_type` whose one input, `port`, is a String setting `read` at `read`.
fn a_node_whose_one_setting_is_read(
    node_type: &str,
    read: crate::core::descriptors::SettingReadTime,
) -> crate::core::graph::GraphNode {
    let setting = crate::core::graph::PortInfo::from(
        &crate::core::descriptors::PortDescriptor::new(
            "port",
            "",
            false,
            crate::core::descriptors::PortFamilyDeclaration::of_family(
                crate::core::descriptors::PortFamily::Records,
            ),
        )
        .with_pin_declaration(crate::core::descriptors::PinDeclaration::exactly(
            crate::core::descriptors::PinType::String {},
        ))
        .with_input_mode(crate::core::descriptors::NodeInputMode::SettingOnly { read }),
    );
    crate::core::graph::GraphNode::new(
        streamlib_node_schema::NodeClassImportPath::new(node_type).expect("a legal import path"),
        Some(crate::core::descriptors::NodeKind::Process),
        "Labeller",
        serde_json::Map::new(),
        vec![setting],
        Vec::new(),
    )
}

/// Each input `graph` serves says whether it takes a wire as the link check rules: a setting
/// read at setup takes one, a setting read each run takes none, and neither does any setting of
/// a node type of the graph itself; an output says nothing of it.
///
/// Mental-revert: reading the mark from the mode alone says the `GraphInput`'s `port` takes one;
/// exempting a `GraphInput` alone says the `GetVariable`'s does.
#[test]
fn each_input_the_graph_serves_says_whether_it_takes_a_wire() {
    use crate::core::descriptors::SettingReadTime;
    let takes_a_wire = |node_type: &str, read| {
        serde_json::to_value(GraphNodeOutput::from(&a_node_whose_one_setting_is_read(
            node_type, read,
        )))
        .unwrap()["ports"]["inputs"][0]["takes_a_wire"]
            .clone()
    };

    assert_eq!(
        takes_a_wire("tests:Labeller", SettingReadTime::AtSetup),
        serde_json::json!(true)
    );
    assert_eq!(
        takes_a_wire("tests:Labeller", SettingReadTime::EachRun),
        serde_json::json!(false)
    );
    for node_type in [
        streamlib_project_repository::GRAPH_INPUT_NODE_TYPE,
        streamlib_project_repository::GRAPH_OUTPUT_NODE_TYPE,
        streamlib_project_repository::SET_VARIABLE_NODE_TYPE,
        streamlib_project_repository::GET_VARIABLE_NODE_TYPE,
    ] {
        assert_eq!(
            takes_a_wire(node_type, SettingReadTime::AtSetup),
            serde_json::json!(false),
            "{node_type}"
        );
    }
    let output = serde_json::to_value(GraphNodeOutput::from(&a_node_whose_output_is(
        crate::core::descriptors::PortFamily::Records,
        None,
        None,
    )))
    .unwrap();
    assert!(
        output["ports"]["outputs"][0].get("takes_a_wire").is_none(),
        "{output}"
    );
}

#[test]
fn a_node_whose_type_nothing_registers_renders_no_kind() {
    let unregistered = serde_json::to_value(GraphNodeOutput::from(&a_node_of_kind(None))).unwrap();
    assert!(unregistered.get("kind").is_none(), "{unregistered}");
    assert!(unregistered.get("language").is_none(), "{unregistered}");
}

#[test]
fn a_catalog_entry_renders_its_kind_and_a_function_node_its_language_beside_it() {
    use crate::core::descriptors::{FunctionNodeLanguage, NodeKind};
    let mut descriptor = crate::core::NodeDescriptor::new(
        crate::core::descriptors::NodeClassShortName::new("Lfo").expect("a short name"),
        streamlib_node_schema::NodeClassImportPath::new("tests:Lfo").expect("a legal import path"),
        "oscillates",
    );
    let process = serde_json::to_value(NodeDescriptorOutput::from(&descriptor)).unwrap();
    assert_eq!(process["kind"], "process");
    assert!(process.get("language").is_none(), "{process}");

    descriptor.node_kind = NodeKind::Function {
        language: FunctionNodeLanguage::Rust,
    };
    let function = serde_json::to_value(NodeDescriptorOutput::from(&descriptor)).unwrap();
    assert_eq!(function["kind"], "function");
    assert_eq!(function["language"], "rust");
}

#[test]
fn an_image_output_renders_the_frame_rate_its_config_declares_as_its_stream_facts() {
    let rendered = serde_json::to_value(GraphNodeOutput::from(&a_node_whose_output_is(
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
    let unstated = serde_json::to_value(GraphNodeOutput::from(&node)).unwrap();
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
    let replaced = serde_json::to_value(GraphNodeOutput::from(&node)).unwrap();
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
    let rendered = serde_json::to_value(GraphNodeOutput::from(&node)).unwrap();
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
        delivery_profile: None,
        borrow_quota: None,
        audio_window: None,
        pin_default: None,
        pin: None,
        declared_pin_type: None,
        lifetime: crate::core::descriptors::NodeValueLifetime::Kept,
        former_names: Vec::new(),
        may_be_unset: false,
        family: crate::core::descriptors::PortFamilyDeclaration::of_family(
            crate::core::descriptors::PortFamily::Image,
        ),
        input_mode: None,
        annotations: Default::default(),
    };
    let json = serde_json::to_value(GraphNodePortOnTheWire::from(&port)).unwrap();
    assert_carries_no_type_key(&json);
}

#[test]
fn a_picture_ports_descriptor_renders_its_pin_type_and_flow_and_no_retired_type_key() {
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
    assert_renders_exactly(&json, &PORT_DESCRIPTOR_OF_A_PICTURE_KEYS);
    assert_carries_no_type_key(&json);
}

#[test]
fn a_typed_ports_descriptor_renders_the_range_unit_and_meaning_its_pin_declares() {
    let mut declaration = crate::core::descriptors::PinDeclaration::exactly(
        crate::core::descriptors::PinType::Float { exact_size: None },
    );
    declaration.annotations.range = Some(crate::core::descriptors::PinNumberRange {
        minimum: Some(20.0),
        maximum: Some(20000.0),
    });
    declaration.annotations.unit = Some("Hz".to_owned());
    declaration.annotations.meaning = Some("frequency".to_owned());
    let port = crate::core::PortDescriptor::new(
        "frequency",
        "",
        true,
        crate::core::descriptors::PortFamilyDeclaration::of_family(
            crate::core::descriptors::PortFamily::Channels,
        ),
    )
    .with_pin_declaration(declaration);

    let json = serde_json::to_value(PortDescriptorOutput::from(&port)).unwrap();

    assert_eq!(json["pin_type"]["pin_type"]["type"], "float");
    assert_eq!(
        json["annotations"],
        serde_json::json!({
            "range": {"minimum": 20.0, "maximum": 20000.0},
            "unit": "Hz",
            "meaning": "frequency",
        })
    );
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
        delivery_profile: Some("ordered".to_string()),
        borrow_quota: None,
        audio_window: Some(declared_window_contract()),
        pin_default: None,
        pin: None,
        declared_pin_type: None,
        lifetime: crate::core::descriptors::NodeValueLifetime::Kept,
        former_names: Vec::new(),
        may_be_unset: false,
        family: crate::core::descriptors::PortFamilyDeclaration::of_family(
            crate::core::descriptors::PortFamily::Image,
        ),
        input_mode: None,
        annotations: Default::default(),
    };
    let json = serde_json::to_value(GraphNodePortOnTheWire::from(&port)).unwrap();

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
        pin: None,
        declared_pin_type: None,
        lifetime: crate::core::descriptors::NodeValueLifetime::Kept,
        former_names: Vec::new(),
        may_be_unset: false,
        family: crate::core::descriptors::PortFamilyDeclaration::of_family(
            crate::core::descriptors::PortFamily::Image,
        ),
        input_mode: None,
        annotations: Default::default(),
    };
    let json = serde_json::to_value(GraphNodePortOnTheWire::from(&port)).unwrap();

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
        delivery_profile: Some("ordered".to_string()),
        borrow_quota: None,
        audio_window: Some(crate::core::descriptors::AudioWindowContract::MatchDevice {}),
        pin_default: None,
        pin: None,
        declared_pin_type: None,
        lifetime: crate::core::descriptors::NodeValueLifetime::Kept,
        former_names: Vec::new(),
        may_be_unset: false,
        family: crate::core::descriptors::PortFamilyDeclaration::of_family(
            crate::core::descriptors::PortFamily::Image,
        ),
        input_mode: None,
        annotations: Default::default(),
    };
    let json = serde_json::to_value(GraphNodePortOnTheWire::from(&port)).unwrap();

    assert_eq!(
        json["audio_window"],
        serde_json::json!({ "resolved_from": "match_device" })
    );
}

/// A node whose input port declares the sentinel, with `settled` optionally
/// standing in for what its own device gave it.
fn a_node_declaring_the_sentinel(
    settled: Option<crate::iceoryx2::ResolvedAudioWindowContract>,
) -> crate::core::graph::GraphNode {
    use crate::core::graph::GraphNodeWithComponents;

    let mut node = crate::core::graph::GraphNode::new(
        streamlib_node_schema::NodeClassImportPath::new("tests.SpeakerSink")
            .expect("a legal import path"),
        Some(crate::core::descriptors::NodeKind::Process),
        "SpeakerSink",
        serde_json::Map::new(),
        vec![crate::core::graph::PortInfo {
            name: "audio".to_string(),
            description: "Blocks to play".to_string(),
            delivery_profile: Some("ordered".to_string()),
            borrow_quota: None,
            audio_window: Some(crate::core::descriptors::AudioWindowContract::MatchDevice {}),
            pin_default: None,
            pin: None,
            declared_pin_type: None,
            lifetime: crate::core::descriptors::NodeValueLifetime::Kept,
            former_names: Vec::new(),
            may_be_unset: false,
            family: crate::core::descriptors::PortFamilyDeclaration::of_family(
                crate::core::descriptors::PortFamily::Image,
            ),
            input_mode: None,
            annotations: Default::default(),
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

    let rendered = serde_json::to_value(GraphNodeOutput::from(&a_node_declaring_the_sentinel(
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

#[test]
fn an_unsettled_match_device_port_still_renders_the_sentinel() {
    let rendered =
        serde_json::to_value(GraphNodeOutput::from(&a_node_declaring_the_sentinel(None))).unwrap();

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
    let rendered =
        serde_json::to_value(GraphNodeOutput::from(&a_node_declaring_the_sentinel(None))).unwrap();

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
            crate::core::descriptors::PortFamily::Channels,
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
            crate::core::descriptors::PortFamily::Channels,
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
