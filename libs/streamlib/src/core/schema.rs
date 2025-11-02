//! Schema system for AI-discoverable processors
//!
//! This module provides schema definitions and metadata for processors,
//! enabling AI agents to:
//! - Discover available processors
//! - Understand what each processor does (descriptions, context)
//! - Validate connections (schema compatibility)
//! - Generate code that uses processors correctly
//!
//! # Design Philosophy
//!
//! Schemas serve two purposes:
//! 1. **Runtime validation**: Ensure connections are type-safe
//! 2. **AI discovery**: Let agents understand capabilities and usage
//!
//! # Example
//!
//! ```ignore
//! use streamlib_core::schema::{Schema, ProcessorDescriptor, PortDescriptor};
//!
//! // Define processor metadata
//! let descriptor = ProcessorDescriptor {
//!     name: "ObjectDetector".into(),
//!     description: "Detects objects using YOLOv8".into(),
//!     usage_context: Some("Use for identifying objects in real-time video".into()),
//!     inputs: vec![
//!         PortDescriptor {
//!             name: "video".into(),
//!             schema: SCHEMA_VIDEO_FRAME.clone(),
//!             required: true,
//!             description: "Input video frame to analyze".into(),
//!         }
//!     ],
//!     outputs: vec![
//!         PortDescriptor {
//!             name: "detections".into(),
//!             schema: detection_schema(),
//!             required: true,
//!             description: "Detected objects with bounding boxes".into(),
//!         }
//!     ],
//!     examples: vec![],
//!     tags: vec!["ml".into(), "vision".into(), "detection".into()],
//! };
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

/// Semantic version for schema evolution
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl SemanticVersion {
    pub fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self { major, minor, patch }
    }

    /// Check if this version is compatible with another
    /// Compatible = same major version, this minor >= other minor
    pub fn compatible_with(&self, other: &SemanticVersion) -> bool {
        self.major == other.major && self.minor >= other.minor
    }
}

impl std::fmt::Display for SemanticVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl From<&str> for SemanticVersion {
    fn from(s: &str) -> Self {
        let parts: Vec<&str> = s.split('.').collect();
        Self {
            major: parts.first().and_then(|s| s.parse().ok()).unwrap_or(0),
            minor: parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0),
            patch: parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0),
        }
    }
}

/// Field type in a schema
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FieldType {
    // Primitive types
    Int32,
    Int64,
    UInt32,
    UInt64,
    Float32,
    Float64,
    Bool,
    String,
    Bytes,

    // Composite types
    Array(Box<FieldType>),
    Struct(Vec<Field>),
    Enum(Vec<String>),
    Optional(Box<FieldType>),

    // GPU types (references to GPU resources)
    Texture {
        format: String, // "RGBA8", "BGRA8", etc.
    },
    Buffer {
        element_type: Box<FieldType>,
    },

    // Reference to another schema
    SchemaRef(String),
}

/// Field definition in a schema
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    pub field_type: FieldType,
    pub required: bool,

    /// Human-readable description for AI agents
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Additional metadata (units, constraints, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

impl Field {
    pub fn new(name: impl Into<String>, field_type: FieldType) -> Self {
        Self {
            name: name.into(),
            field_type,
            required: true,
            description: None,
            metadata: None,
        }
    }

    pub fn optional(mut self) -> Self {
        self.required = false;
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata
            .get_or_insert_with(HashMap::new)
            .insert(key.into(), value);
        self
    }
}

/// Serialization format for schema data
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SerializationFormat {
    /// Protocol Buffers (compact, fast, cross-language)
    Protobuf,
    /// Apache Arrow (columnar, great for ML data)
    Arrow,
    /// Bincode (fast Rust serialization)
    Bincode,
    /// JSON (human-readable, debugging)
    Json,
    /// MessagePack (compact, fast)
    MessagePack,
}

/// Schema definition
///
/// Defines the structure of data flowing through ports.
/// Used for validation and AI discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schema {
    /// Schema name (e.g., "VideoFrame", "ObjectDetections")
    pub name: String,

    /// Schema version (semantic versioning)
    pub version: SemanticVersion,

    /// Human-readable description for AI agents
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Fields in this schema
    pub fields: Vec<Field>,

    /// Preferred serialization format
    pub serialization: SerializationFormat,

    /// Additional metadata
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

impl Schema {
    /// Create a new schema
    pub fn new(
        name: impl Into<String>,
        version: SemanticVersion,
        fields: Vec<Field>,
        serialization: SerializationFormat,
    ) -> Self {
        Self {
            name: name.into(),
            version,
            description: None,
            fields,
            serialization,
            metadata: None,
        }
    }

    /// Add description (builder pattern)
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Check if this schema is compatible with another
    pub fn compatible_with(&self, other: &Schema) -> bool {
        // Same name
        if self.name != other.name {
            return false;
        }

        // Compatible version
        if !self.version.compatible_with(&other.version) {
            return false;
        }

        // All required fields in 'other' must exist in 'self'
        for required_field in other.fields.iter().filter(|f| f.required) {
            if !self.has_compatible_field(required_field) {
                return false;
            }
        }

        true
    }

    /// Check if schema has a field compatible with the given field
    fn has_compatible_field(&self, field: &Field) -> bool {
        self.fields.iter().any(|f| {
            f.name == field.name && Self::types_compatible(&f.field_type, &field.field_type)
        })
    }

    /// Check if two field types are compatible
    fn types_compatible(a: &FieldType, b: &FieldType) -> bool {
        use FieldType::*;

        match (a, b) {
            // Exact matches
            (Int32, Int32) | (Int64, Int64) | (UInt32, UInt32) | (UInt64, UInt64) => true,
            (Float32, Float32) | (Float64, Float64) => true,
            (Bool, Bool) | (String, String) | (Bytes, Bytes) => true,

            // Arrays
            (Array(a), Array(b)) => Self::types_compatible(a, b),

            // Optionals
            (Optional(a), Optional(b)) => Self::types_compatible(a, b),
            (Optional(a), b) => Self::types_compatible(a, b), // Optional can accept required
            (a, Optional(b)) => Self::types_compatible(a, b),

            // Enums (must have same variants)
            (Enum(a), Enum(b)) => a == b,

            // Structs (recursively check fields)
            (Struct(a_fields), Struct(b_fields)) => {
                b_fields.iter().filter(|f| f.required).all(|b_field| {
                    a_fields.iter().any(|a_field| {
                        a_field.name == b_field.name
                            && Self::types_compatible(&a_field.field_type, &b_field.field_type)
                    })
                })
            }

            // GPU types
            (Texture { format: f1 }, Texture { format: f2 }) => f1 == f2,
            (Buffer { element_type: e1 }, Buffer { element_type: e2 }) => {
                Self::types_compatible(e1, e2)
            }

            // Schema refs (assume compatible if names match)
            (SchemaRef(a), SchemaRef(b)) => a == b,

            _ => false,
        }
    }

    /// Get detailed compatibility error (for debugging)
    pub fn compatibility_error(&self, other: &Schema) -> String {
        if self.name != other.name {
            return format!("Schema name mismatch: {} vs {}", self.name, other.name);
        }

        if !self.version.compatible_with(&other.version) {
            return format!(
                "Version incompatible: {} not compatible with {}",
                self.version, other.version
            );
        }

        for required_field in other.fields.iter().filter(|f| f.required) {
            if !self.has_compatible_field(required_field) {
                return format!(
                    "Missing or incompatible required field: {} ({:?})",
                    required_field.name, required_field.field_type
                );
            }
        }

        "Unknown compatibility issue".into()
    }

    /// Export schema to JSON (for AI agents)
    ///
    /// Returns compact JSON (single line) for efficient transmission over MCP.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Export schema to YAML (for AI agents)
    pub fn to_yaml(&self) -> Result<String, serde_yaml::Error> {
        serde_yaml::to_string(self)
    }
}

/// Port descriptor with schema and metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortDescriptor {
    /// Port name (e.g., "video", "detections")
    pub name: String,

    /// Schema for this port
    #[serde(
        serialize_with = "serialize_arc_schema",
        deserialize_with = "deserialize_arc_schema"
    )]
    pub schema: Arc<Schema>,

    /// Whether this port is required
    pub required: bool,

    /// Human-readable description for AI agents
    /// Example: "Input video frame to analyze for objects"
    pub description: String,
}

// Custom serialization for Arc<Schema>
fn serialize_arc_schema<S>(schema: &Arc<Schema>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    schema.as_ref().serialize(serializer)
}

fn deserialize_arc_schema<'de, D>(deserializer: D) -> Result<Arc<Schema>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Schema::deserialize(deserializer).map(Arc::new)
}

impl PortDescriptor {
    pub fn new(
        name: impl Into<String>,
        schema: Arc<Schema>,
        required: bool,
        description: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            schema,
            required,
            description: description.into(),
        }
    }
}

/// Example showing typical processor usage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessorExample {
    /// Description of what this example demonstrates
    pub description: String,

    /// Example input data (JSON representation)
    pub input_example: serde_json::Value,

    /// Example output data (JSON representation)
    pub output_example: serde_json::Value,
}

impl ProcessorExample {
    pub fn new(
        description: impl Into<String>,
        input_example: serde_json::Value,
        output_example: serde_json::Value,
    ) -> Self {
        Self {
            description: description.into(),
            input_example,
            output_example,
        }
    }
}

/// Audio processing requirements for a processor
///
/// Processors declare their audio requirements so the runtime can:
/// - Validate compatibility when connecting processors
/// - Provide correct configuration to agents via MCP
/// - Insert automatic adapters when needed (future)
///
/// # Example
///
/// ```ignore
/// AudioRequirements {
///     preferred_buffer_size: Some(2048),  // Efficient size
///     required_buffer_size: None,         // But flexible
///     supported_sample_rates: vec![44100, 48000],
///     required_channels: Some(2),         // Stereo only
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioRequirements {
    /// Preferred buffer size in samples per channel
    ///
    /// This is the most efficient size for this processor, but it can
    /// adapt to other sizes if needed.
    ///
    /// Example: 2048 samples (standard audio plugin size)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_buffer_size: Option<usize>,

    /// Required buffer size in samples per channel
    ///
    /// If set, this processor ONLY works with this exact buffer size.
    /// The runtime will validate connections and may insert adapters.
    ///
    /// Example: Some CLAP plugins require specific buffer sizes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_buffer_size: Option<usize>,

    /// Supported sample rates in Hz
    ///
    /// Empty = any sample rate is supported.
    /// Non-empty = only these specific rates work.
    ///
    /// Example: vec![44100, 48000, 96000]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub supported_sample_rates: Vec<u32>,

    /// Required number of audio channels
    ///
    /// None = any channel count is supported.
    /// Some(n) = only this specific channel count works.
    ///
    /// Example: Some(2) for stereo-only processors
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_channels: Option<u32>,
}

impl AudioRequirements {
    /// Create audio requirements with no restrictions (flexible)
    pub fn flexible() -> Self {
        Self {
            preferred_buffer_size: None,
            required_buffer_size: None,
            supported_sample_rates: Vec::new(),
            required_channels: None,
        }
    }

    /// Create audio requirements with preferred settings
    pub fn with_preferred(buffer_size: usize, sample_rate: u32, channels: u32) -> Self {
        Self {
            preferred_buffer_size: Some(buffer_size),
            required_buffer_size: None,
            supported_sample_rates: vec![sample_rate],
            required_channels: Some(channels),
        }
    }

    /// Create strict audio requirements (required, not just preferred)
    pub fn required(buffer_size: usize, sample_rate: u32, channels: u32) -> Self {
        Self {
            preferred_buffer_size: None,
            required_buffer_size: Some(buffer_size),
            supported_sample_rates: vec![sample_rate],
            required_channels: Some(channels),
        }
    }

    /// Check if this processor's requirements are compatible with another's
    ///
    /// Returns true if data can flow from this processor to the other.
    pub fn compatible_with(&self, downstream: &AudioRequirements) -> bool {
        // Check buffer size compatibility
        if let (Some(our_size), Some(their_size)) =
            (self.required_buffer_size, downstream.required_buffer_size) {
            if our_size != their_size {
                return false;
            }
        }

        // Check sample rate compatibility
        if !downstream.supported_sample_rates.is_empty()
            && !self.supported_sample_rates.is_empty() {
            let has_common_rate = downstream.supported_sample_rates.iter()
                .any(|rate| self.supported_sample_rates.contains(rate));
            if !has_common_rate {
                return false;
            }
        }

        // Check channel compatibility
        if let (Some(our_channels), Some(their_channels)) =
            (self.required_channels, downstream.required_channels) {
            if our_channels != their_channels {
                return false;
            }
        }

        true
    }

    /// Get detailed compatibility error message
    pub fn compatibility_error(&self, downstream: &AudioRequirements) -> String {
        // Check buffer size
        if let (Some(our_size), Some(their_size)) =
            (self.required_buffer_size, downstream.required_buffer_size) {
            if our_size != their_size {
                return format!(
                    "Buffer size mismatch: upstream outputs {} samples, downstream requires {}",
                    our_size, their_size
                );
            }
        }

        // Check sample rate
        if !downstream.supported_sample_rates.is_empty()
            && !self.supported_sample_rates.is_empty() {
            let has_common_rate = downstream.supported_sample_rates.iter()
                .any(|rate| self.supported_sample_rates.contains(rate));
            if !has_common_rate {
                return format!(
                    "Sample rate mismatch: upstream supports {:?}, downstream requires {:?}",
                    self.supported_sample_rates, downstream.supported_sample_rates
                );
            }
        }

        // Check channels
        if let (Some(our_channels), Some(their_channels)) =
            (self.required_channels, downstream.required_channels) {
            if our_channels != their_channels {
                return format!(
                    "Channel count mismatch: upstream outputs {} channels, downstream requires {}",
                    our_channels, their_channels
                );
            }
        }

        "Audio requirements are compatible".to_string()
    }
}

/// Timer requirements for processors that need periodic wakeups
///
/// Processors like displays need to refresh at fixed rates (e.g., 60 Hz rendering).
/// This declares timer requirements for event-driven architecture.
///
/// # Example
///
/// ```ignore
/// // Display processor with independent timer
/// ProcessorDescriptor::new("DisplayProcessor", "Renders video to window")
///     .with_timer_requirements(TimerRequirements {
///         rate_hz: 60.0,  // 60 FPS rendering
///         group_id: None,  // Independent timer
///         description: Some("Display refresh rate".into()),
///     });
///
/// // Audio generator in synchronized timer group
/// ProcessorDescriptor::new("TestToneGenerator", "Generates test tones")
///     .with_timer_requirements(TimerRequirements {
///         rate_hz: 23.44,  // 48000 Hz / 2048 samples
///         group_id: Some("audio_master".to_string()),  // Share timer with other audio processors
///         description: Some("Audio generation clock".into()),
///     })
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerRequirements {
    /// Timer rate in Hz (e.g., 60.0 for 60 FPS, 23.44 for audio)
    pub rate_hz: f64,

    /// Optional timer group ID (clock domain name)
    ///
    /// Processors with the same group_id share a master timer thread,
    /// ensuring synchronized wake-ups with no clock drift.
    ///
    /// Use cases:
    /// - Multiple audio generators feeding a mixer: "audio_master"
    /// - Synchronized video sources: "video_60fps"
    /// - Multi-camera stereo rig: "stereo_cameras"
    ///
    /// If None, processor gets an independent timer (default behavior).
    ///
    /// Inspired by:
    /// - GStreamer's pipeline clock
    /// - Core Audio's clock domains
    /// - PipeWire's clock naming
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,

    /// Description of what the timer is used for
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Processor descriptor for AI discovery
///
/// This contains all metadata needed for AI agents to understand
/// and use a processor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessorDescriptor {
    /// Processor name (e.g., "ObjectDetector", "AudioMixer")
    pub name: String,

    /// Human-readable description for AI agents
    /// Example: "Detects objects in video frames using YOLOv8. Returns bounding boxes with class labels and confidence scores."
    pub description: String,

    /// Usage context (when to use this processor)
    /// Example: "Use when you need to identify objects, people, or animals in real-time video. Works best with clear, well-lit scenes."
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_context: Option<String>,

    /// Input ports
    pub inputs: Vec<PortDescriptor>,

    /// Output ports
    pub outputs: Vec<PortDescriptor>,

    /// Examples of typical usage
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<ProcessorExample>,

    /// Tags for discovery (e.g., ["ml", "vision", "detection"])
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    /// Audio processing requirements (optional)
    ///
    /// If this processor handles audio, declare its requirements here.
    /// This enables:
    /// - Runtime validation of audio pipeline compatibility
    /// - AI agents discovering correct buffer sizes and sample rates
    /// - Automatic adapter insertion (future)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_requirements: Option<AudioRequirements>,

    /// Timer requirements (optional)
    ///
    /// If this processor needs periodic wakeups (e.g., displays at 60 Hz),
    /// declare timer requirements here. The runtime will spawn a dedicated
    /// timer thread that sends WakeupEvent::TimerTick at the requested rate.
    ///
    /// # Example
    ///
    /// ```ignore
    /// ProcessorDescriptor::new("DisplayProcessor", "...")
    ///     .with_timer_requirements(TimerRequirements {
    ///         rate_hz: 60.0,
    ///         description: Some("Display refresh rate".into()),
    ///     })
    /// ```
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timer_requirements: Option<TimerRequirements>,

    /// Additional metadata
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

impl ProcessorDescriptor {
    /// Create a new processor descriptor
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            usage_context: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            examples: Vec::new(),
            tags: Vec::new(),
            audio_requirements: None,
            timer_requirements: None,
            metadata: None,
        }
    }

    /// Add usage context (builder pattern)
    pub fn with_usage_context(mut self, context: impl Into<String>) -> Self {
        self.usage_context = Some(context.into());
        self
    }

    /// Add input port (builder pattern)
    pub fn with_input(mut self, port: PortDescriptor) -> Self {
        self.inputs.push(port);
        self
    }

    /// Add output port (builder pattern)
    pub fn with_output(mut self, port: PortDescriptor) -> Self {
        self.outputs.push(port);
        self
    }

    /// Add example (builder pattern)
    pub fn with_example(mut self, example: ProcessorExample) -> Self {
        self.examples.push(example);
        self
    }

    /// Add tag (builder pattern)
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Add multiple tags (builder pattern)
    pub fn with_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags.extend(tags.into_iter().map(|t| t.into()));
        self
    }

    /// Add audio requirements (builder pattern)
    ///
    /// Declares what audio configuration this processor needs/supports.
    /// This enables runtime validation and helps AI agents generate correct code.
    ///
    /// # Example
    ///
    /// ```ignore
    /// ProcessorDescriptor::new("ClapEffect", "CLAP audio plugin")
    ///     .with_audio_requirements(AudioRequirements::required(2048, 48000, 2))
    /// ```
    pub fn with_audio_requirements(mut self, requirements: AudioRequirements) -> Self {
        self.audio_requirements = Some(requirements);
        self
    }

    /// Add timer requirements (builder pattern)
    ///
    /// Use this for processors that need periodic wakeups at fixed rates.
    /// The runtime will spawn a timer thread that sends WakeupEvent::TimerTick
    /// at the requested rate.
    ///
    /// # Example
    ///
    /// ```ignore
    /// ProcessorDescriptor::new("DisplayProcessor", "Renders video to window")
    ///     .with_timer_requirements(TimerRequirements {
    ///         rate_hz: 60.0,
    ///         description: Some("Display refresh rate".into()),
    ///     })
    /// ```
    pub fn with_timer_requirements(mut self, requirements: TimerRequirements) -> Self {
        self.timer_requirements = Some(requirements);
        self
    }

    /// Export descriptor to JSON (for AI agents)
    ///
    /// Returns compact JSON (single line) for efficient transmission over MCP.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Export descriptor to YAML (for AI agents)
    pub fn to_yaml(&self) -> Result<String, serde_yaml::Error> {
        serde_yaml::to_string(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_semantic_version_compatibility() {
        let v1_0_0 = SemanticVersion::new(1, 0, 0);
        let v1_1_0 = SemanticVersion::new(1, 1, 0);
        let v1_0_1 = SemanticVersion::new(1, 0, 1);
        let v2_0_0 = SemanticVersion::new(2, 0, 0);

        // Same major, newer minor is compatible
        assert!(v1_1_0.compatible_with(&v1_0_0));
        assert!(v1_1_0.compatible_with(&v1_1_0));

        // Same major, older minor is NOT compatible
        assert!(!v1_0_0.compatible_with(&v1_1_0));

        // Patch versions don't affect compatibility
        assert!(v1_0_1.compatible_with(&v1_0_0));
        assert!(v1_0_0.compatible_with(&v1_0_1));

        // Different major is NOT compatible
        assert!(!v2_0_0.compatible_with(&v1_0_0));
        assert!(!v1_0_0.compatible_with(&v2_0_0));
    }

    #[test]
    fn test_schema_compatibility() {
        let schema_v1 = Schema::new(
            "TestSchema",
            SemanticVersion::new(1, 0, 0),
            vec![
                Field::new("id", FieldType::Int32),
                Field::new("name", FieldType::String),
            ],
            SerializationFormat::Json,
        );

        let schema_v1_1 = Schema::new(
            "TestSchema",
            SemanticVersion::new(1, 1, 0),
            vec![
                Field::new("id", FieldType::Int32),
                Field::new("name", FieldType::String),
                Field::new("age", FieldType::Int32).optional(),
            ],
            SerializationFormat::Json,
        );

        // v1.1 can accept v1.0 (backward compatible - same major, higher minor)
        assert!(schema_v1_1.compatible_with(&schema_v1));

        // v1.0 CANNOT accept v1.1 (version incompatible - 1.0 cannot accept 1.1)
        assert!(!schema_v1.compatible_with(&schema_v1_1));
    }

    #[test]
    fn test_field_type_compatibility() {
        use FieldType::*;

        assert!(Schema::types_compatible(&Int32, &Int32));
        assert!(!Schema::types_compatible(&Int32, &Int64));

        // Optional compatibility
        assert!(Schema::types_compatible(
            &Optional(Box::new(Int32)),
            &Int32
        ));
        assert!(Schema::types_compatible(
            &Int32,
            &Optional(Box::new(Int32))
        ));

        // Array compatibility
        assert!(Schema::types_compatible(
            &Array(Box::new(Int32)),
            &Array(Box::new(Int32))
        ));
        assert!(!Schema::types_compatible(
            &Array(Box::new(Int32)),
            &Array(Box::new(Int64))
        ));
    }

    #[test]
    fn test_processor_descriptor_builder() {
        let descriptor = ProcessorDescriptor::new("TestProcessor", "A test processor")
            .with_usage_context("Use for testing")
            .with_tag("test")
            .with_tag("example");

        assert_eq!(descriptor.name, "TestProcessor");
        assert_eq!(descriptor.description, "A test processor");
        assert_eq!(descriptor.usage_context, Some("Use for testing".into()));
        assert_eq!(descriptor.tags, vec!["test", "example"]);
    }

    #[test]
    fn test_schema_serialization() {
        let schema = Schema::new(
            "TestSchema",
            SemanticVersion::new(1, 0, 0),
            vec![
                Field::new("id", FieldType::Int32).with_description("Unique identifier"),
                Field::new("name", FieldType::String).with_description("Name field"),
            ],
            SerializationFormat::Json,
        )
        .with_description("A test schema");

        let json = schema.to_json().expect("Failed to serialize to JSON");
        assert!(json.contains("TestSchema"));
        assert!(json.contains("Unique identifier"));

        let yaml = schema.to_yaml().expect("Failed to serialize to YAML");
        assert!(yaml.contains("TestSchema"));
    }
}

// ============================================================================
// Standard Schema Registry
// ============================================================================
//
// These are the built-in schemas that all processors can reference.
// AI agents can discover these schemas and understand the standard data types.

/// Standard schema for video frames
///
/// Represents a single video frame with GPU texture data.
pub static SCHEMA_VIDEO_FRAME: LazyLock<Arc<Schema>> = LazyLock::new(|| {
    Arc::new(
        Schema::new(
            "VideoFrame",
            SemanticVersion::new(1, 0, 0),
            vec![
                Field::new("texture", FieldType::Texture {
                    format: "RGBA8".into(),
                })
                .with_description("WebGPU texture containing the frame data"),
                Field::new("format", FieldType::String)
                    .with_description("Texture format (RGBA8, BGRA8, etc.)"),
                Field::new("width", FieldType::UInt32)
                    .with_description("Frame width in pixels"),
                Field::new("height", FieldType::UInt32)
                    .with_description("Frame height in pixels"),
                Field::new("timestamp", FieldType::Float64)
                    .with_description("Timestamp in seconds since stream start"),
                Field::new("frame_number", FieldType::UInt64)
                    .with_description("Sequential frame number"),
                Field::new("metadata", FieldType::Optional(Box::new(FieldType::Struct(vec![
                    // Flexible metadata structure
                ]))))
                .optional()
                .with_description("Optional metadata (detection boxes, ML results, etc.)"),
            ],
            SerializationFormat::Bincode,
        )
        .with_description(
            "A single video frame with GPU texture data. This is the standard format for video data flowing through streamlib pipelines."
        ),
    )
});

/// Standard schema for audio frames
///
/// Represents a chunk of audio data with CPU-first architecture.
/// AudioFrame uses CPU storage with optional GPU buffer for flexible audio processing.
pub static SCHEMA_AUDIO_FRAME: LazyLock<Arc<Schema>> = LazyLock::new(|| {
    Arc::new(
        Schema::new(
            "AudioFrame",
            SemanticVersion::new(2, 0, 0),
            vec![
                Field::new("samples", FieldType::Array(Box::new(FieldType::Float32)))
                    .with_description("Interleaved audio samples (f32 in range [-1.0, 1.0])"),
                Field::new("channels", FieldType::UInt32)
                    .with_description("Number of channels (1=mono, 2=stereo, 6=5.1, 8=7.1)"),
                Field::new("timestamp_ns", FieldType::Int64)
                    .with_description("Timestamp in nanoseconds since stream start"),
                Field::new("frame_number", FieldType::UInt64)
                    .with_description("Sequential frame number for drop detection"),
                Field::new("metadata", FieldType::Optional(Box::new(FieldType::Struct(vec![]))))
                    .optional()
                    .with_description("Optional metadata (ML results, labels, etc.)"),
            ],
            SerializationFormat::Bincode,
        )
        .with_description(
            "Fixed-size audio buffer with streaming metadata. Sample rate is enforced by RuntimeContext. \
             Use dasp for zero-copy frame access (stereo, mono, surround)."
        ),
    )
});

/// Standard schema for generic data frames
///
/// For custom data types that don't fit VideoFrame or AudioBuffer.
pub static SCHEMA_DATA_MESSAGE: LazyLock<Arc<Schema>> = LazyLock::new(|| {
    Arc::new(
        Schema::new(
            "DataFrame",
            SemanticVersion::new(1, 0, 0),
            vec![
                Field::new("buffer", FieldType::Buffer {
                    element_type: Box::new(FieldType::Bytes),
                })
                .with_description("WebGPU buffer containing custom data"),
                Field::new("timestamp", FieldType::Float64)
                    .with_description("Timestamp in seconds since stream start"),
                Field::new("metadata", FieldType::Optional(Box::new(FieldType::Struct(vec![]))))
                    .optional()
                    .with_description("Optional metadata describing the data format"),
            ],
            SerializationFormat::Bincode,
        )
        .with_description(
            "Generic data message for custom data types. Use metadata to describe the specific format."
        ),
    )
});

/// Schema for bounding boxes (common in object detection)
pub static SCHEMA_BOUNDING_BOX: LazyLock<Arc<Schema>> = LazyLock::new(|| {
    Arc::new(
        Schema::new(
            "BoundingBox",
            SemanticVersion::new(1, 0, 0),
            vec![
                Field::new("x", FieldType::Float32)
                    .with_description("X coordinate (normalized 0-1 or pixel value)"),
                Field::new("y", FieldType::Float32)
                    .with_description("Y coordinate (normalized 0-1 or pixel value)"),
                Field::new("width", FieldType::Float32)
                    .with_description("Box width (normalized 0-1 or pixel value)"),
                Field::new("height", FieldType::Float32)
                    .with_description("Box height (normalized 0-1 or pixel value)"),
                Field::new("normalized", FieldType::Bool)
                    .with_description("True if coordinates are normalized (0-1), false if pixels"),
            ],
            SerializationFormat::Json,
        )
        .with_description("A rectangular bounding box, commonly used in object detection and tracking."),
    )
});

/// Schema for object detections (ML output)
pub static SCHEMA_OBJECT_DETECTIONS: LazyLock<Arc<Schema>> = LazyLock::new(|| {
    Arc::new(
        Schema::new(
            "ObjectDetections",
            SemanticVersion::new(1, 0, 0),
            vec![
                Field::new("timestamp", FieldType::Float64)
                    .with_description("Timestamp when detections were made"),
                Field::new("objects", FieldType::Array(Box::new(FieldType::Struct(vec![
                    Field::new("class", FieldType::String)
                        .with_description("Object class label (e.g., 'person', 'car')"),
                    Field::new("confidence", FieldType::Float32)
                        .with_description("Detection confidence (0.0 to 1.0)"),
                    Field::new("bbox", FieldType::SchemaRef("BoundingBox".into()))
                        .with_description("Bounding box coordinates"),
                ]))))
                .with_description("Array of detected objects"),
            ],
            SerializationFormat::Json,
        )
        .with_description(
            "Object detection results from ML models. Contains bounding boxes, class labels, and confidence scores."
        ),
    )
});
