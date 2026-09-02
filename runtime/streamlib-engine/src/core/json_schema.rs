// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! JSON Schema output types for API documentation.
//!
//! These structs mirror the serialization output of the runtime types and are used
//! for generating JSON Schema files. They implement both `Serialize` and `JsonSchema`
//! to ensure schemas stay in sync with actual serialization.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::core::graph::{GraphEdgeWithComponents, GraphNodeWithComponents};

/// The processor-identity wire type. Defined in the engine-free
/// `streamlib-processor-schema` crate so the MoQ catalog and the authoring
/// chain share one definition; re-exported here so the
/// `streamlib::sdk::json_schema` facade the API server consumes resolves it.
/// The `utoipa` feature the engine enables gives it the `utoipa::ToSchema`
/// derive the aggregate response types below require.
pub use streamlib_processor_schema::ProcessorClassImportPath;

// =============================================================================
// Graph Response Schema (/api/graph)
// =============================================================================

/// Response from the `/api/graph` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct GraphResponse {
    /// All processor nodes in the graph.
    pub nodes: Vec<ProcessorNodeOutput>,
    /// All links (connections) between processors.
    pub links: Vec<LinkOutput>,
}

/// A processor node in the graph.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct ProcessorNodeOutput {
    /// Unique identifier for this processor instance.
    pub id: String,
    /// The import path of the class this processor is — a plain string.
    #[serde(rename = "type")]
    pub processor_type: ProcessorClassImportPath,
    /// Display name for UI. May differ from type for hosted processors.
    pub display_name: String,
    /// Processor configuration as JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    /// Checksum of config for change detection.
    #[serde(default)]
    pub config_checksum: u64,
    /// Input and output ports.
    pub ports: ProcessorNodePortsOutput,
    /// Runtime components (dynamic, varies based on processor state).
    pub components: serde_json::Map<String, serde_json::Value>,
}

/// Container for processor input and output ports.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct ProcessorNodePortsOutput {
    /// Input ports that receive data.
    pub inputs: Vec<PortInfoOutput>,
    /// Output ports that send data.
    pub outputs: Vec<PortInfoOutput>,
}

/// Metadata about a port.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct PortInfoOutput {
    /// Port name (e.g., "video_in", "audio_out").
    pub name: String,
    /// Human-readable description declared alongside the port.
    #[serde(default)]
    pub description: String,
    /// Kind of port: data, event, or control.
    #[serde(default)]
    pub port_kind: PortKindOutput,
    /// Delivery profile declared by this input port — `"newest"` or
    /// `"ordered"`; `None` on an output port.
    pub delivery_profile: Option<String>,
    /// Window contract declared by this audio input port. Absent from the
    /// rendering on a port that declares none — the contract is opt-in, and a
    /// port without one renders exactly what it always did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_window: Option<crate::core::descriptors::AudioWindowContract>,
}

/// The kind of port - determines how data flows.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum PortKindOutput {
    #[default]
    Data,
    Event,
    Control,
}

/// A link (connection) between two processor ports.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct LinkOutput {
    /// Unique identifier for this link.
    pub id: String,
    /// Source endpoint (output port).
    pub source: LinkPortRefOutput,
    /// Target endpoint (input port).
    pub target: LinkPortRefOutput,
    /// Ring buffer capacity for the channel.
    #[serde(default)]
    pub capacity: usize,
    /// Current state of the link.
    #[serde(default)]
    pub state: LinkStateOutput,
    /// Runtime components (dynamic, varies based on link state).
    pub components: serde_json::Map<String, serde_json::Value>,
}

/// Reference to a port on a processor.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct LinkPortRefOutput {
    /// Processor instance ID.
    pub processor_id: String,
    /// Port name on that processor.
    pub port_name: String,
}

/// State of a link in the graph.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum LinkStateOutput {
    /// Link exists in graph but not yet wired.
    #[default]
    Pending,
    /// Link is actively wired with a ring buffer channel.
    Wired,
    /// Link is being disconnected.
    Disconnecting,
    /// Link was disconnected.
    Disconnected,
    /// Link is in error state.
    Error,
}

// =============================================================================
// Registry Response Schema (/api/registry)
// =============================================================================

/// Response from the `/api/registry` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct RegistryResponse {
    /// Available processor types with their descriptors.
    pub processors: Vec<ProcessorDescriptorOutput>,
}

/// Runtime environment for a processor.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ProcessorRuntimeOutput {
    #[default]
    Rust,
    Python,
}

/// Descriptor for a processor type.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct ProcessorDescriptorOutput {
    /// The import path of the class this processor is.
    ///
    /// The same value a graph node carries, but under its own key: a node
    /// renames the field to `type`, because there it is the node's type; here
    /// it is what the registry is keyed on.
    pub processor_class_import_path: ProcessorClassImportPath,
    /// Human-readable description.
    pub description: String,
    /// Repository URL.
    pub repository: String,
    /// Runtime environment.
    #[serde(default)]
    pub runtime: ProcessorRuntimeOutput,
    /// Entrypoint for non-Rust runtimes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    /// Reference to config schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<String>,
    /// Input port descriptors.
    pub inputs: Vec<PortDescriptorOutput>,
    /// Output port descriptors.
    pub outputs: Vec<PortDescriptorOutput>,
    /// Code examples in different languages.
    pub examples: CodeExamplesOutput,
}

/// A configuration field for a processor.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct ConfigFieldOutput {
    /// Field name.
    pub name: String,
    /// Field type as string (e.g., "String", "u32", "Option<PathBuf>").
    #[serde(rename = "type")]
    pub field_type: String,
    /// Whether the field is required.
    pub required: bool,
    /// Human-readable description.
    pub description: String,
}

/// Descriptor for a processor port.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct PortDescriptorOutput {
    /// Port name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Whether the port is required.
    pub required: bool,
    /// Delivery profile declared by this input port — `"newest"` or
    /// `"ordered"`; `None` on an output port.
    pub delivery_profile: Option<String>,
    /// Window contract declared by this audio input port. Absent from the
    /// rendering on a port that declares none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_window: Option<crate::core::descriptors::AudioWindowContract>,
}

/// Code examples for a processor in different languages.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct CodeExamplesOutput {
    /// Rust code example.
    pub rust: String,
    /// Python code example.
    pub python: String,
    /// TypeScript code example.
    pub typescript: String,
}

// =============================================================================
// Conversion from Runtime Types
// =============================================================================

impl From<&crate::core::graph::ProcessorNode> for ProcessorNodeOutput {
    fn from(node: &crate::core::graph::ProcessorNode) -> Self {
        Self {
            id: node.id.to_string(),
            processor_type: node.processor_type.clone(),
            display_name: node.display_name.clone(),
            config: node.config.clone(),
            config_checksum: node.config_checksum,
            ports: ProcessorNodePortsOutput::rendered_for_a_node_over_its_settled_contracts(node),
            components: node.serialize_components(),
        }
    }
}

impl ProcessorNodePortsOutput {
    /// This node's ports, with any `match_device` contract its own device
    /// stream has settled rendered as the five values it settled to.
    ///
    /// A port declaring the sentinel renders it until its processor's `setup()`
    /// opens a device, and the resolved values from then on: machine-dependent
    /// because the device format is, which is truer than a static lie.
    ///
    /// Takes the node rather than its ports, and is the only way they are
    /// rendered: the settled values live beside the ports rather than in them,
    /// so a renderer handed the ports alone could only ever produce the
    /// sentinel — and would look like the right call to reach for.
    fn rendered_for_a_node_over_its_settled_contracts(
        node: &crate::core::graph::ProcessorNode,
    ) -> Self {
        let settled = node
            .get::<crate::core::graph::DeviceMatchedAudioWindowContractsComponent>()
            .map(|component| &*component.0);
        Self {
            inputs: node
                .ports
                .inputs
                .iter()
                .map(|port| PortInfoOutput::rendered_over_any_settled_contract(port, settled))
                .collect(),
            outputs: node
                .ports
                .outputs
                .iter()
                .map(PortInfoOutput::from)
                .collect(),
        }
    }
}

impl PortInfoOutput {
    /// One input port, with a `match_device` declaration replaced by whatever
    /// its own device stream settled it to.
    fn rendered_over_any_settled_contract(
        port: &crate::core::graph::PortInfo,
        settled: Option<&crate::iceoryx2::DeviceMatchedAudioWindowContractsByInputPort>,
    ) -> Self {
        let mut rendered = PortInfoOutput::from(port);
        if !matches!(
            rendered.audio_window,
            Some(crate::core::descriptors::AudioWindowContract::MatchDevice {})
        ) {
            return rendered;
        }
        if let Some(values) =
            settled.and_then(|contracts| contracts.settled_declaration_for_input_port(&port.name))
        {
            rendered.audio_window = Some(crate::core::descriptors::AudioWindowContract::Device(
                values,
            ));
        }
        rendered
    }
}

impl From<&crate::core::graph::PortInfo> for PortInfoOutput {
    fn from(port: &crate::core::graph::PortInfo) -> Self {
        Self {
            name: port.name.clone(),
            description: port.description.clone(),
            port_kind: PortKindOutput::from(port.port_kind),
            delivery_profile: port.delivery_profile.clone(),
            audio_window: port.audio_window.clone(),
        }
    }
}

impl From<crate::core::graph::PortKind> for PortKindOutput {
    fn from(kind: crate::core::graph::PortKind) -> Self {
        match kind {
            crate::core::graph::PortKind::Data => PortKindOutput::Data,
            crate::core::graph::PortKind::Event => PortKindOutput::Event,
            crate::core::graph::PortKind::Control => PortKindOutput::Control,
        }
    }
}

impl From<&crate::core::graph::Link> for LinkOutput {
    fn from(link: &crate::core::graph::Link) -> Self {
        Self {
            id: link.id.to_string(),
            source: LinkPortRefOutput::from(&link.source),
            target: LinkPortRefOutput::from(&link.target),
            capacity: link.capacity.get(),
            state: LinkStateOutput::from(link.state),
            components: link.serialize_components(),
        }
    }
}

impl From<&crate::core::graph::OutputLinkPortRef> for LinkPortRefOutput {
    fn from(port_ref: &crate::core::graph::OutputLinkPortRef) -> Self {
        Self {
            processor_id: port_ref.processor_id.to_string(),
            port_name: port_ref.port_name.clone(),
        }
    }
}

impl From<&crate::core::graph::InputLinkPortRef> for LinkPortRefOutput {
    fn from(port_ref: &crate::core::graph::InputLinkPortRef) -> Self {
        Self {
            processor_id: port_ref.processor_id.to_string(),
            port_name: port_ref.port_name.clone(),
        }
    }
}

impl From<crate::core::graph::LinkState> for LinkStateOutput {
    fn from(state: crate::core::graph::LinkState) -> Self {
        match state {
            crate::core::graph::LinkState::Pending => LinkStateOutput::Pending,
            crate::core::graph::LinkState::Wired => LinkStateOutput::Wired,
            crate::core::graph::LinkState::Disconnecting => LinkStateOutput::Disconnecting,
            crate::core::graph::LinkState::Disconnected => LinkStateOutput::Disconnected,
            crate::core::graph::LinkState::Error => LinkStateOutput::Error,
        }
    }
}

impl From<&crate::core::ProcessorDescriptor> for ProcessorDescriptorOutput {
    fn from(desc: &crate::core::ProcessorDescriptor) -> Self {
        Self {
            processor_class_import_path: desc.processor_class_import_path.clone(),
            description: desc.description.clone(),
            repository: desc.repository.clone(),
            runtime: ProcessorRuntimeOutput::from(&desc.runtime),
            entrypoint: desc.entrypoint.clone(),
            config_schema: desc.config_schema.clone(),
            inputs: desc.inputs.iter().map(PortDescriptorOutput::from).collect(),
            outputs: desc
                .outputs
                .iter()
                .map(PortDescriptorOutput::from)
                .collect(),
            examples: CodeExamplesOutput::from(&desc.examples),
        }
    }
}

impl From<&crate::core::ProcessorRuntime> for ProcessorRuntimeOutput {
    fn from(runtime: &crate::core::ProcessorRuntime) -> Self {
        match runtime {
            crate::core::ProcessorRuntime::Rust => ProcessorRuntimeOutput::Rust,
            crate::core::ProcessorRuntime::Python => ProcessorRuntimeOutput::Python,
        }
    }
}

impl From<&crate::core::ConfigField> for ConfigFieldOutput {
    fn from(field: &crate::core::ConfigField) -> Self {
        Self {
            name: field.name.clone(),
            field_type: field.field_type.clone(),
            required: field.required,
            description: field.description.clone(),
        }
    }
}

impl From<&crate::core::PortDescriptor> for PortDescriptorOutput {
    fn from(port: &crate::core::PortDescriptor) -> Self {
        Self {
            name: port.name.clone(),
            description: port.description.clone(),
            required: port.required,
            delivery_profile: port.delivery_profile.clone(),
            audio_window: port.audio_window.clone(),
        }
    }
}

impl From<&crate::core::CodeExamples> for CodeExamplesOutput {
    fn from(examples: &crate::core::CodeExamples) -> Self {
        Self {
            rust: examples.rust.clone(),
            python: examples.python.clone(),
            typescript: examples.typescript.clone(),
        }
    }
}

#[cfg(test)]
mod port_rendering_tests {
    use super::*;

    /// Every key a rendered port may carry. A port that grows a type field
    /// again fails here, whatever the field is named.
    const PORT_INFO_KEYS: [&str; 4] = ["name", "description", "port_kind", "delivery_profile"];
    const PORT_DESCRIPTOR_KEYS: [&str; 4] = ["name", "description", "required", "delivery_profile"];
    const PORT_INFO_WITH_A_CONTRACT_KEYS: [&str; 5] = [
        "name",
        "description",
        "port_kind",
        "delivery_profile",
        "audio_window",
    ];
    const PORT_DESCRIPTOR_WITH_A_CONTRACT_KEYS: [&str; 5] = [
        "name",
        "description",
        "required",
        "delivery_profile",
        "audio_window",
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
            audio_window: None,
        };
        let json = serde_json::to_value(PortInfoOutput::from(&port)).unwrap();

        assert_eq!(json["name"], "video_in");
        assert_eq!(json["description"], "Frames to convert");
        assert_eq!(json["port_kind"], "data");
        assert_eq!(json["delivery_profile"], "newest");
        assert_renders_exactly(&json, &PORT_INFO_KEYS);
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
            audio_window: None,
        };
        let json = serde_json::to_value(PortInfoOutput::from(&port)).unwrap();
        assert_carries_no_type_key(&json);
    }

    #[test]
    fn port_descriptor_output_carries_no_type_key() {
        let pd = crate::core::PortDescriptor::new("video", "Video output", true)
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
            audio_window: Some(declared_window_contract()),
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
            audio_window: Some(
                crate::core::descriptors::AudioWindowContract::Declaration(
                    crate::core::descriptors::AudioWindowContractDeclaredValues {
                        sample_rate: 48_000,
                        channels: None,
                        dtype: "f32".to_string(),
                        window_size: 960,
                        hop: 960,
                    },
                ),
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
            audio_window: Some(crate::core::descriptors::AudioWindowContract::MatchDevice {}),
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
            "SpeakerSink",
            None,
            vec![crate::core::graph::PortInfo {
                name: "audio".to_string(),
                description: "Blocks to play".to_string(),
                port_kind: crate::core::graph::PortKind::Data,
                delivery_profile: Some("ordered".to_string()),
                audio_window: Some(crate::core::descriptors::AudioWindowContract::MatchDevice {}),
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

        let rendered = serde_json::to_value(ProcessorNodeOutput::from(
            &a_node_declaring_the_sentinel(Some(settled)),
        ))
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
        let rendered = serde_json::to_value(ProcessorNodeOutput::from(
            &a_node_declaring_the_sentinel(None),
        ))
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
        let rendered = serde_json::to_value(ProcessorNodeOutput::from(
            &a_node_declaring_the_sentinel(None),
        ))
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
        let descriptor = crate::core::PortDescriptor::iceoryx2("audio", "Samples to frame")
            .with_delivery_profile("ordered")
            .with_audio_window_contract(declared_window_contract());

        let port = crate::core::graph::PortInfo::from(&descriptor);

        assert_eq!(port.audio_window, Some(declared_window_contract()));
    }

    #[test]
    fn a_contract_bearing_descriptor_renders_its_contract_too() {
        let descriptor = crate::core::PortDescriptor::iceoryx2("audio", "Samples to frame")
            .with_delivery_profile("ordered")
            .with_audio_window_contract(
                crate::core::descriptors::AudioWindowContract::MatchDevice {},
            );
        let json = serde_json::to_value(PortDescriptorOutput::from(&descriptor)).unwrap();

        assert_eq!(
            json["audio_window"],
            serde_json::json!({ "resolved_from": "match_device" })
        );
        assert_renders_exactly(&json, &PORT_DESCRIPTOR_WITH_A_CONTRACT_KEYS);
    }
}
