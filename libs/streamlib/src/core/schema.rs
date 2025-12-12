// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl SemanticVersion {
    pub fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FieldType {
    Int32,
    Int64,
    UInt32,
    UInt64,
    Float32,
    Float64,
    Bool,
    String,
    Bytes,

    Array(Box<FieldType>),
    Struct(Vec<Field>),
    Enum(Vec<String>),
    Optional(Box<FieldType>),

    Texture {
        format: String, // "RGBA8", "BGRA8", etc.
    },
    Buffer {
        element_type: Box<FieldType>,
    },

    SchemaRef(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    pub field_type: FieldType,
    pub required: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SerializationFormat {
    Protobuf,
    Arrow,
    Bincode,
    Json,
    MessagePack,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schema {
    pub name: String,

    pub version: SemanticVersion,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    pub fields: Vec<Field>,

    pub serialization: SerializationFormat,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

impl Schema {
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

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn compatible_with(&self, other: &Schema) -> bool {
        if self.name != other.name {
            return false;
        }

        if !self.version.compatible_with(&other.version) {
            return false;
        }

        for required_field in other.fields.iter().filter(|f| f.required) {
            if !self.has_compatible_field(required_field) {
                return false;
            }
        }

        true
    }

    fn has_compatible_field(&self, field: &Field) -> bool {
        self.fields.iter().any(|f| {
            f.name == field.name && Self::types_compatible(&f.field_type, &field.field_type)
        })
    }

    fn types_compatible(a: &FieldType, b: &FieldType) -> bool {
        use FieldType::*;

        match (a, b) {
            (Int32, Int32) | (Int64, Int64) | (UInt32, UInt32) | (UInt64, UInt64) => true,
            (Float32, Float32) | (Float64, Float64) => true,
            (Bool, Bool) | (String, String) | (Bytes, Bytes) => true,

            (Array(a), Array(b)) => Self::types_compatible(a, b),

            (Optional(a), Optional(b)) => Self::types_compatible(a, b),
            (Optional(a), b) => Self::types_compatible(a, b), // Optional can accept required
            (a, Optional(b)) => Self::types_compatible(a, b),

            (Enum(a), Enum(b)) => a == b,

            (Struct(a_fields), Struct(b_fields)) => {
                b_fields.iter().filter(|f| f.required).all(|b_field| {
                    a_fields.iter().any(|a_field| {
                        a_field.name == b_field.name
                            && Self::types_compatible(&a_field.field_type, &b_field.field_type)
                    })
                })
            }

            (Texture { format: f1 }, Texture { format: f2 }) => f1 == f2,
            (Buffer { element_type: e1 }, Buffer { element_type: e2 }) => {
                Self::types_compatible(e1, e2)
            }

            (SchemaRef(a), SchemaRef(b)) => a == b,

            _ => false,
        }
    }

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

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn to_yaml(&self) -> Result<String, serde_yaml::Error> {
        serde_yaml::to_string(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortDescriptor {
    pub name: String,

    #[serde(
        serialize_with = "serialize_arc_schema",
        deserialize_with = "deserialize_arc_schema"
    )]
    pub schema: Arc<Schema>,

    pub required: bool,

    pub description: String,
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessorExample {
    pub description: String,

    pub input_example: serde_json::Value,

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioRequirements {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_buffer_size: Option<usize>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_buffer_size: Option<usize>,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub supported_sample_rates: Vec<u32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_channels: Option<u32>,
}

impl AudioRequirements {
    pub fn flexible() -> Self {
        Self {
            preferred_buffer_size: None,
            required_buffer_size: None,
            supported_sample_rates: Vec::new(),
            required_channels: None,
        }
    }

    pub fn with_preferred(buffer_size: usize, sample_rate: u32, channels: u32) -> Self {
        Self {
            preferred_buffer_size: Some(buffer_size),
            required_buffer_size: None,
            supported_sample_rates: vec![sample_rate],
            required_channels: Some(channels),
        }
    }

    pub fn required(buffer_size: usize, sample_rate: u32, channels: u32) -> Self {
        Self {
            preferred_buffer_size: None,
            required_buffer_size: Some(buffer_size),
            supported_sample_rates: vec![sample_rate],
            required_channels: Some(channels),
        }
    }

    pub fn compatible_with(&self, downstream: &AudioRequirements) -> bool {
        if let (Some(our_size), Some(their_size)) =
            (self.required_buffer_size, downstream.required_buffer_size)
        {
            if our_size != their_size {
                return false;
            }
        }

        if !downstream.supported_sample_rates.is_empty() && !self.supported_sample_rates.is_empty()
        {
            let has_common_rate = downstream
                .supported_sample_rates
                .iter()
                .any(|rate| self.supported_sample_rates.contains(rate));
            if !has_common_rate {
                return false;
            }
        }

        if let (Some(our_channels), Some(their_channels)) =
            (self.required_channels, downstream.required_channels)
        {
            if our_channels != their_channels {
                return false;
            }
        }

        true
    }

    pub fn compatibility_error(&self, downstream: &AudioRequirements) -> String {
        if let (Some(our_size), Some(their_size)) =
            (self.required_buffer_size, downstream.required_buffer_size)
        {
            if our_size != their_size {
                return format!(
                    "Buffer size mismatch: upstream outputs {} samples, downstream requires {}",
                    our_size, their_size
                );
            }
        }

        if !downstream.supported_sample_rates.is_empty() && !self.supported_sample_rates.is_empty()
        {
            let has_common_rate = downstream
                .supported_sample_rates
                .iter()
                .any(|rate| self.supported_sample_rates.contains(rate));
            if !has_common_rate {
                return format!(
                    "Sample rate mismatch: upstream supports {:?}, downstream requires {:?}",
                    self.supported_sample_rates, downstream.supported_sample_rates
                );
            }
        }

        if let (Some(our_channels), Some(their_channels)) =
            (self.required_channels, downstream.required_channels)
        {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessorDescriptor {
    pub name: String,

    pub description: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_context: Option<String>,

    pub inputs: Vec<PortDescriptor>,

    pub outputs: Vec<PortDescriptor>,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<ProcessorExample>,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_requirements: Option<AudioRequirements>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

impl ProcessorDescriptor {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            usage_context: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            examples: Vec::new(),
            tags: Vec::new(),
            audio_requirements: None,
            metadata: None,
        }
    }

    pub fn with_usage_context(mut self, context: impl Into<String>) -> Self {
        self.usage_context = Some(context.into());
        self
    }

    pub fn with_input(mut self, port: PortDescriptor) -> Self {
        self.inputs.push(port);
        self
    }

    pub fn with_output(mut self, port: PortDescriptor) -> Self {
        self.outputs.push(port);
        self
    }

    pub fn with_example(mut self, example: ProcessorExample) -> Self {
        self.examples.push(example);
        self
    }

    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    pub fn with_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags.extend(tags.into_iter().map(|t| t.into()));
        self
    }

    pub fn with_audio_requirements(mut self, requirements: AudioRequirements) -> Self {
        self.audio_requirements = Some(requirements);
        self
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn to_yaml(&self) -> Result<String, serde_yaml::Error> {
        serde_yaml::to_string(self)
    }
}

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
                Field::new("timestamp_ns", FieldType::Int64)
                    .with_description("Timestamp in nanoseconds from MediaClock (monotonic time)"),
                Field::new("frame_number", FieldType::UInt64)
                    .with_description("Sequential frame number"),
                Field::new("metadata", FieldType::Optional(Box::new(FieldType::Struct(vec![
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
        .with_description(
            "A rectangular bounding box, commonly used in object detection and tracking.",
        ),
    )
});

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_semantic_version_compatibility() {
        let v1_0_0 = SemanticVersion::new(1, 0, 0);
        let v1_1_0 = SemanticVersion::new(1, 1, 0);
        let v1_0_1 = SemanticVersion::new(1, 0, 1);
        let v2_0_0 = SemanticVersion::new(2, 0, 0);

        assert!(v1_1_0.compatible_with(&v1_0_0));
        assert!(v1_1_0.compatible_with(&v1_1_0));

        assert!(!v1_0_0.compatible_with(&v1_1_0));

        assert!(v1_0_1.compatible_with(&v1_0_0));
        assert!(v1_0_0.compatible_with(&v1_0_1));

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

        assert!(schema_v1_1.compatible_with(&schema_v1));

        assert!(!schema_v1.compatible_with(&schema_v1_1));
    }

    #[test]
    fn test_field_type_compatibility() {
        use FieldType::*;

        assert!(Schema::types_compatible(&Int32, &Int32));
        assert!(!Schema::types_compatible(&Int32, &Int64));

        assert!(Schema::types_compatible(&Optional(Box::new(Int32)), &Int32));
        assert!(Schema::types_compatible(&Int32, &Optional(Box::new(Int32))));

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
