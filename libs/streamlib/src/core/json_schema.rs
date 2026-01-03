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

// =============================================================================
// Graph Response Schema (/api/graph)
// =============================================================================

/// Response from the `/api/graph` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GraphResponse {
    /// All processor nodes in the graph.
    pub nodes: Vec<ProcessorNodeOutput>,
    /// All links (connections) between processors.
    pub links: Vec<LinkOutput>,
}

/// A processor node in the graph.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProcessorNodeOutput {
    /// Unique identifier for this processor instance.
    pub id: String,
    /// The processor type name (e.g., "CameraProcessor", "DisplayProcessor").
    #[serde(rename = "type")]
    pub processor_type: String,
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProcessorNodePortsOutput {
    /// Input ports that receive data.
    pub inputs: Vec<PortInfoOutput>,
    /// Output ports that send data.
    pub outputs: Vec<PortInfoOutput>,
}

/// Metadata about a port.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PortInfoOutput {
    /// Port name (e.g., "video_in", "audio_out").
    pub name: String,
    /// Data type flowing through this port.
    pub data_type: String,
    /// Kind of port: data, event, or control.
    #[serde(default)]
    pub port_kind: PortKindOutput,
}

/// The kind of port - determines how data flows.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PortKindOutput {
    #[default]
    Data,
    Event,
    Control,
}

/// A link (connection) between two processor ports.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LinkPortRefOutput {
    /// Processor instance ID.
    pub processor_id: String,
    /// Port name on that processor.
    pub port_name: String,
}

/// State of a link in the graph.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema)]
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
    /// Available data frame schemas.
    pub schemas: Vec<SchemaDescriptorOutput>,
}

/// Descriptor for a processor type.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct ProcessorDescriptorOutput {
    /// Processor type name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Semantic version string.
    pub version: String,
    /// Repository URL.
    pub repository: String,
    /// Configuration fields.
    pub config: Vec<ConfigFieldOutput>,
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
    /// Schema name for data flowing through this port.
    pub schema: String,
    /// Whether the port is required.
    pub required: bool,
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

/// Descriptor for a data schema.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct SchemaDescriptorOutput {
    /// Schema name (e.g., "VideoFrame", "AudioFrame").
    pub name: String,
    /// Semantic version.
    pub version: SemanticVersionOutput,
    /// Fields in this schema.
    pub fields: Vec<SchemaFieldOutput>,
    /// How data is read from the link buffer.
    pub read_behavior: LinkBufferReadModeOutput,
    /// Default ring buffer capacity.
    pub default_capacity: usize,
}

/// Semantic version (major.minor.patch).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct SemanticVersionOutput {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

/// A field in a data schema.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
pub struct SchemaFieldOutput {
    /// Field name.
    pub name: String,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Type name (e.g., "u32", "Arc<wgpu::Texture>").
    #[serde(rename = "type")]
    pub type_name: String,
    /// Shape for multi-dimensional fields (e.g., [512] for embeddings).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shape: Vec<usize>,
    /// Whether this is an internal field (not serializable).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub internal: bool,
}

/// How data is read from the link buffer.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LinkBufferReadModeOutput {
    /// Drain buffer and return only the newest frame (optimal for video).
    #[default]
    SkipToLatest,
    /// Read next frame in FIFO order (required for audio).
    ReadNextInOrder,
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
            ports: ProcessorNodePortsOutput::from(&node.ports),
            components: node.serialize_components(),
        }
    }
}

impl From<&crate::core::graph::ProcessorNodePorts> for ProcessorNodePortsOutput {
    fn from(ports: &crate::core::graph::ProcessorNodePorts) -> Self {
        Self {
            inputs: ports.inputs.iter().map(PortInfoOutput::from).collect(),
            outputs: ports.outputs.iter().map(PortInfoOutput::from).collect(),
        }
    }
}

impl From<&crate::core::graph::PortInfo> for PortInfoOutput {
    fn from(port: &crate::core::graph::PortInfo) -> Self {
        Self {
            name: port.name.clone(),
            data_type: port.data_type.clone(),
            port_kind: PortKindOutput::from(port.port_kind),
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

impl From<&crate::core::schema::ProcessorDescriptor> for ProcessorDescriptorOutput {
    fn from(desc: &crate::core::schema::ProcessorDescriptor) -> Self {
        Self {
            name: desc.name.clone(),
            description: desc.description.clone(),
            version: desc.version.clone(),
            repository: desc.repository.clone(),
            config: desc.config.iter().map(ConfigFieldOutput::from).collect(),
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

impl From<&crate::core::schema::ConfigField> for ConfigFieldOutput {
    fn from(field: &crate::core::schema::ConfigField) -> Self {
        Self {
            name: field.name.clone(),
            field_type: field.field_type.clone(),
            required: field.required,
            description: field.description.clone(),
        }
    }
}

impl From<&crate::core::schema::PortDescriptor> for PortDescriptorOutput {
    fn from(port: &crate::core::schema::PortDescriptor) -> Self {
        Self {
            name: port.name.clone(),
            description: port.description.clone(),
            schema: port.schema.clone(),
            required: port.required,
        }
    }
}

impl From<&crate::core::schema::CodeExamples> for CodeExamplesOutput {
    fn from(examples: &crate::core::schema::CodeExamples) -> Self {
        Self {
            rust: examples.rust.clone(),
            python: examples.python.clone(),
            typescript: examples.typescript.clone(),
        }
    }
}

impl From<&crate::core::schema_registry::SchemaDescriptor> for SchemaDescriptorOutput {
    fn from(desc: &crate::core::schema_registry::SchemaDescriptor) -> Self {
        Self {
            name: desc.name.clone(),
            version: SemanticVersionOutput::from(&desc.version),
            fields: desc.fields.iter().map(SchemaFieldOutput::from).collect(),
            read_behavior: LinkBufferReadModeOutput::from(desc.read_behavior),
            default_capacity: desc.default_capacity,
        }
    }
}

impl From<&crate::core::schema::SemanticVersion> for SemanticVersionOutput {
    fn from(version: &crate::core::schema::SemanticVersion) -> Self {
        Self {
            major: version.major,
            minor: version.minor,
            patch: version.patch,
        }
    }
}

impl From<&crate::core::schema::DataFrameSchemaField> for SchemaFieldOutput {
    fn from(field: &crate::core::schema::DataFrameSchemaField) -> Self {
        Self {
            name: field.name.clone(),
            description: field.description.clone(),
            type_name: field.type_name.clone(),
            shape: field.shape.clone(),
            internal: field.internal,
        }
    }
}

impl From<crate::core::links::LinkBufferReadMode> for LinkBufferReadModeOutput {
    fn from(mode: crate::core::links::LinkBufferReadMode) -> Self {
        match mode {
            crate::core::links::LinkBufferReadMode::SkipToLatest => {
                LinkBufferReadModeOutput::SkipToLatest
            }
            crate::core::links::LinkBufferReadMode::ReadNextInOrder => {
                LinkBufferReadModeOutput::ReadNextInOrder
            }
        }
    }
}
