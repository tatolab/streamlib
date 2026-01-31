// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Schema definition types.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ============================================================================
// Processor Schema Types
// ============================================================================

/// Runtime language for a processor.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProcessorLanguage {
    #[default]
    Rust,
    Python,
    TypeScript,
}

/// Language-specific runtime options.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeOptions {
    /// [Rust] Generate unsafe Send impl for !Send processors (AVFoundation, etc.)
    #[serde(default)]
    pub unsafe_send: bool,
    /// [Python] Required Python version spec (e.g., ">=3.10"). Python runtime only.
    #[serde(default)]
    pub python_version: Option<String>,
}

/// Internal helper for deserializing RuntimeConfig from either string or object.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum RuntimeConfigHelper {
    /// Simple string form: `runtime: rust`
    Simple(ProcessorLanguage),
    /// Full object form: `runtime: { language: rust, options: { unsafe_send: true } }`
    Full(RuntimeConfigFull),
}

/// Full runtime configuration object.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct RuntimeConfigFull {
    /// Language runtime (rust, python, typescript). Defaults to rust.
    #[serde(default)]
    pub language: ProcessorLanguage,

    /// Language-specific options.
    #[serde(default)]
    pub options: RuntimeOptions,

    /// Environment variables for subprocess runtimes.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

/// Runtime configuration for a processor.
///
/// Supports flexible YAML formats:
/// - Simple string: `runtime: rust` (defaults to no options)
/// - Object form: `runtime: { language: rust, options: { unsafe_send: true } }`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeConfig {
    /// Language runtime (rust, python, typescript). Defaults to rust.
    pub language: ProcessorLanguage,

    /// Language-specific options.
    pub options: RuntimeOptions,

    /// Environment variables for subprocess runtimes.
    pub env: std::collections::HashMap<String, String>,
}

impl From<RuntimeConfigHelper> for RuntimeConfig {
    fn from(helper: RuntimeConfigHelper) -> Self {
        match helper {
            RuntimeConfigHelper::Simple(language) => RuntimeConfig {
                language,
                options: RuntimeOptions::default(),
                env: std::collections::HashMap::new(),
            },
            RuntimeConfigHelper::Full(full) => RuntimeConfig {
                language: full.language,
                options: full.options,
                env: full.env,
            },
        }
    }
}

impl Serialize for RuntimeConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // If only language is set with defaults, serialize as simple string
        if self.options == RuntimeOptions::default() && self.env.is_empty() {
            return self.language.serialize(serializer);
        }

        // Otherwise serialize as full object
        let full = RuntimeConfigFull {
            language: self.language.clone(),
            options: self.options.clone(),
            env: self.env.clone(),
        };
        full.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RuntimeConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        RuntimeConfigHelper::deserialize(deserializer).map(RuntimeConfig::from)
    }
}

/// Execution mode for a processor.
///
/// Supports flexible YAML formats:
/// - Simple string: `execution: reactive` or `execution: manual`
/// - Object for continuous with interval: `execution: { type: continuous, interval_ms: 10 }`
/// - Object for continuous default interval: `execution: continuous` (interval_ms defaults to 0)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ProcessExecution {
    /// Reactive: process() is called when input arrives.
    #[default]
    Reactive,
    /// Manual: start()/stop() control execution.
    Manual,
    /// Continuous: runs in a loop calling process() repeatedly.
    Continuous {
        /// Minimum interval between process() calls in milliseconds.
        /// Default is 0 (as fast as possible).
        interval_ms: u32,
    },
}

impl ProcessExecution {
    /// Returns the interval_ms for Continuous mode, or None for other modes.
    pub fn interval_ms(&self) -> Option<u32> {
        match self {
            ProcessExecution::Continuous { interval_ms } => Some(*interval_ms),
            _ => None,
        }
    }
}

impl Serialize for ProcessExecution {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        match self {
            ProcessExecution::Reactive => serializer.serialize_str("reactive"),
            ProcessExecution::Manual => serializer.serialize_str("manual"),
            ProcessExecution::Continuous { interval_ms } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "continuous")?;
                map.serialize_entry("interval_ms", interval_ms)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ProcessExecution {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{self, MapAccess, Visitor};

        struct ProcessExecutionVisitor;

        impl<'de> Visitor<'de> for ProcessExecutionVisitor {
            type Value = ProcessExecution;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str(
                    "a string ('reactive', 'manual', 'continuous') or an object with 'type' field",
                )
            }

            fn visit_str<E>(self, value: &str) -> Result<ProcessExecution, E>
            where
                E: de::Error,
            {
                match value.to_lowercase().as_str() {
                    "reactive" => Ok(ProcessExecution::Reactive),
                    "manual" => Ok(ProcessExecution::Manual),
                    "continuous" => Ok(ProcessExecution::Continuous { interval_ms: 0 }),
                    _ => Err(de::Error::unknown_variant(
                        value,
                        &["reactive", "manual", "continuous"],
                    )),
                }
            }

            fn visit_map<M>(self, mut map: M) -> Result<ProcessExecution, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut type_field: Option<String> = None;
                let mut interval_ms: Option<u32> = None;

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "type" => {
                            type_field = Some(map.next_value()?);
                        }
                        "interval_ms" => {
                            interval_ms = Some(map.next_value()?);
                        }
                        _ => {
                            let _: serde::de::IgnoredAny = map.next_value()?;
                        }
                    }
                }

                let type_val = type_field.ok_or_else(|| de::Error::missing_field("type"))?;
                match type_val.to_lowercase().as_str() {
                    "reactive" => Ok(ProcessExecution::Reactive),
                    "manual" => Ok(ProcessExecution::Manual),
                    "continuous" => Ok(ProcessExecution::Continuous {
                        interval_ms: interval_ms.unwrap_or(0),
                    }),
                    _ => Err(de::Error::unknown_variant(
                        &type_val,
                        &["reactive", "manual", "continuous"],
                    )),
                }
            }
        }

        deserializer.deserialize_any(ProcessExecutionVisitor)
    }
}

/// A port definition within a processor schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessorPortSchema {
    /// Port name (e.g., "video_in").
    pub name: String,
    /// Schema reference with version (e.g., "com.tatolab.videoframe@1.0.0").
    pub schema: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,
}

/// Config definition within a processor schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessorConfigSchema {
    /// Config field name (e.g., "config").
    pub name: String,
    /// Schema reference with version (e.g., "com.example.blur.config@1.0.0").
    pub schema: String,
}

/// A state field definition within a processor schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessorStateField {
    /// Field name (e.g., "buffer").
    pub name: String,
    /// Rust type (e.g., "Vec<f32>", "u32", "u64").
    #[serde(rename = "type")]
    pub field_type: String,
    /// Default value expression (e.g., "Vec::new()", "0", "0").
    #[serde(default)]
    pub default: Option<String>,
}

/// A complete processor schema definition parsed from YAML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessorSchema {
    /// Processor name in reverse domain notation (e.g., "com.example.blur").
    pub name: String,

    /// Processor version (e.g., "1.0.0").
    pub version: String,

    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,

    /// Runtime configuration (language, options, env).
    #[serde(default)]
    pub runtime: RuntimeConfig,

    /// Entrypoint for non-Rust runtimes (e.g., "src.blur:BlurProcessor").
    #[serde(default)]
    pub entrypoint: Option<String>,

    /// Execution mode (reactive, manual, continuous).
    #[serde(default)]
    pub execution: ProcessExecution,

    /// Config schema reference.
    #[serde(default)]
    pub config: Option<ProcessorConfigSchema>,

    /// State fields (internal processor state, Default-initialized).
    #[serde(default)]
    pub state: Vec<ProcessorStateField>,

    /// Input port definitions.
    #[serde(default)]
    pub inputs: Vec<ProcessorPortSchema>,

    /// Output port definitions.
    #[serde(default)]
    pub outputs: Vec<ProcessorPortSchema>,
}

impl ProcessorSchema {
    /// Returns the full processor name with version (e.g., "com.example.blur@1.0.0").
    pub fn full_name(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }

    /// Computes the processor ID (hash of full name).
    pub fn processor_id(&self) -> u64 {
        compute_schema_id(&self.full_name())
    }

    /// Returns the Rust struct name derived from the processor name.
    ///
    /// Example: "com.example.blur" -> "Blur"
    pub fn rust_struct_name(&self) -> String {
        let last_segment = self.name.split('.').next_back().unwrap_or(&self.name);
        to_pascal_case(last_segment)
    }

    /// Returns the Rust module name derived from the processor name.
    ///
    /// Example: "com.example.blur" -> "com_example_blur"
    pub fn rust_module_name(&self) -> String {
        self.name.replace('.', "_").to_lowercase()
    }
}

// ============================================================================
// Message Schema Types
// ============================================================================

/// A complete schema definition parsed from YAML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaDefinition {
    /// Schema name (e.g., "com.tatolab.videoframe")
    pub name: String,

    /// Schema version (e.g., "1.0.0")
    pub version: String,

    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,

    /// Fields in the schema.
    #[serde(default)]
    pub fields: Vec<Field>,
}

impl SchemaDefinition {
    /// Returns the full schema name with version (e.g., "com.tatolab.videoframe@1.0.0").
    pub fn full_name(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }

    /// Computes the schema ID (hash of full name).
    pub fn schema_id(&self) -> u64 {
        compute_schema_id(&self.full_name())
    }

    /// Returns the Rust struct name derived from the schema name.
    ///
    /// Example: "com.tatolab.videoframe" -> "VideoFrame"
    pub fn rust_struct_name(&self) -> String {
        // Take the last segment and convert to PascalCase
        let last_segment = self.name.split('.').next_back().unwrap_or(&self.name);
        to_pascal_case(last_segment)
    }

    /// Returns the Rust module name derived from the schema name.
    ///
    /// Example: "com.tatolab.videoframe@1.0.0" -> "com_tatolab_videoframe"
    pub fn rust_module_name(&self) -> String {
        self.name.replace('.', "_").to_lowercase()
    }
}

/// A field within a schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Field {
    /// Field name.
    pub name: String,

    /// Field type.
    #[serde(rename = "type")]
    pub field_type: FieldType,

    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,

    /// Nested fields (for object types).
    #[serde(default)]
    pub fields: Vec<Field>,
}

/// Supported field types in schemas.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    // Primitive types
    String,
    Bool,
    Int8,
    Int16,
    Int32,
    Int64,
    Uint8,
    Uint16,
    Uint32,
    Uint64,
    Float32,
    Float64,
    Bytes,

    // Complex types (parsed from strings like "array<uint8>")
    #[serde(untagged)]
    Complex(String),
}

impl FieldType {
    /// Parse a type string that may contain generics.
    ///
    /// Examples:
    /// - "string" -> FieldType::String
    /// - "array<uint8>" -> FieldType::Complex("array<uint8>")
    /// - "optional<string>" -> FieldType::Complex("optional<string>")
    /// - "map<string,int32>" -> FieldType::Complex("map<string,int32>")
    /// - "object" -> FieldType::Complex("object")
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "string" => FieldType::String,
            "bool" => FieldType::Bool,
            "int8" => FieldType::Int8,
            "int16" => FieldType::Int16,
            "int32" => FieldType::Int32,
            "int64" => FieldType::Int64,
            "uint8" => FieldType::Uint8,
            "uint16" => FieldType::Uint16,
            "uint32" => FieldType::Uint32,
            "uint64" => FieldType::Uint64,
            "float32" => FieldType::Float32,
            "float64" => FieldType::Float64,
            "bytes" => FieldType::Bytes,
            _ => FieldType::Complex(s.to_string()),
        }
    }

    /// Returns the Rust type string for this field type.
    pub fn to_rust_type(&self, nested_struct_name: Option<&str>) -> String {
        match self {
            FieldType::String => "String".to_string(),
            FieldType::Bool => "bool".to_string(),
            FieldType::Int8 => "i8".to_string(),
            FieldType::Int16 => "i16".to_string(),
            FieldType::Int32 => "i32".to_string(),
            FieldType::Int64 => "i64".to_string(),
            FieldType::Uint8 => "u8".to_string(),
            FieldType::Uint16 => "u16".to_string(),
            FieldType::Uint32 => "u32".to_string(),
            FieldType::Uint64 => "u64".to_string(),
            FieldType::Float32 => "f32".to_string(),
            FieldType::Float64 => "f64".to_string(),
            FieldType::Bytes => "Vec<u8>".to_string(),
            FieldType::Complex(s) => parse_complex_rust_type(s, nested_struct_name),
        }
    }

    /// Returns the Python type string for this field type.
    pub fn to_python_type(&self, nested_class_name: Option<&str>) -> String {
        match self {
            FieldType::String => "str".to_string(),
            FieldType::Bool => "bool".to_string(),
            FieldType::Int8
            | FieldType::Int16
            | FieldType::Int32
            | FieldType::Int64
            | FieldType::Uint8
            | FieldType::Uint16
            | FieldType::Uint32
            | FieldType::Uint64 => "int".to_string(),
            FieldType::Float32 | FieldType::Float64 => "float".to_string(),
            FieldType::Bytes => "bytes".to_string(),
            FieldType::Complex(s) => parse_complex_python_type(s, nested_class_name),
        }
    }

    /// Returns the TypeScript type string for this field type.
    pub fn to_typescript_type(&self, nested_interface_name: Option<&str>) -> String {
        match self {
            FieldType::String => "string".to_string(),
            FieldType::Bool => "boolean".to_string(),
            FieldType::Int8
            | FieldType::Int16
            | FieldType::Int32
            | FieldType::Int64
            | FieldType::Uint8
            | FieldType::Uint16
            | FieldType::Uint32
            | FieldType::Uint64
            | FieldType::Float32
            | FieldType::Float64 => "number".to_string(),
            FieldType::Bytes => "Uint8Array".to_string(),
            FieldType::Complex(s) => parse_complex_typescript_type(s, nested_interface_name),
        }
    }
}

/// Compute schema ID from full name (first 8 bytes of SHA-256).
pub fn compute_schema_id(full_name: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(full_name.as_bytes());
    let result = hasher.finalize();
    u64::from_be_bytes(result[0..8].try_into().unwrap())
}

/// Convert a string to PascalCase.
fn to_pascal_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = true;

    for c in s.chars() {
        if c == '_' || c == '-' || c == '.' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(c.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }

    result
}

/// Convert a string to snake_case.
pub fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    let mut prev_was_upper = false;

    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 && !prev_was_upper {
                result.push('_');
            }
            result.push(c.to_ascii_lowercase());
            prev_was_upper = true;
        } else {
            result.push(c);
            prev_was_upper = false;
        }
    }

    result
}

/// Parse complex type string for Rust.
fn parse_complex_rust_type(s: &str, nested_struct_name: Option<&str>) -> String {
    let s_lower = s.to_lowercase();

    if s_lower == "object" {
        return nested_struct_name
            .unwrap_or("serde_json::Value")
            .to_string();
    }

    if let Some(inner) = s_lower
        .strip_prefix("array<")
        .and_then(|s| s.strip_suffix('>'))
    {
        let inner_type = FieldType::parse(inner).to_rust_type(None);
        return format!("Vec<{}>", inner_type);
    }

    if let Some(inner) = s_lower
        .strip_prefix("optional<")
        .and_then(|s| s.strip_suffix('>'))
    {
        let inner_type = FieldType::parse(inner).to_rust_type(None);
        return format!("Option<{}>", inner_type);
    }

    if let Some(inner) = s_lower
        .strip_prefix("map<")
        .and_then(|s| s.strip_suffix('>'))
    {
        if let Some((key, value)) = inner.split_once(',') {
            let key_type = FieldType::parse(key.trim()).to_rust_type(None);
            let value_type = FieldType::parse(value.trim()).to_rust_type(None);
            return format!("std::collections::HashMap<{}, {}>", key_type, value_type);
        }
    }

    // Unknown complex type, return as-is (will cause compile error if invalid)
    s.to_string()
}

/// Parse complex type string for Python.
fn parse_complex_python_type(s: &str, nested_class_name: Option<&str>) -> String {
    let s_lower = s.to_lowercase();

    if s_lower == "object" {
        return nested_class_name.unwrap_or("dict").to_string();
    }

    if let Some(inner) = s_lower
        .strip_prefix("array<")
        .and_then(|s| s.strip_suffix('>'))
    {
        let inner_type = FieldType::parse(inner).to_python_type(None);
        return format!("list[{}]", inner_type);
    }

    if let Some(inner) = s_lower
        .strip_prefix("optional<")
        .and_then(|s| s.strip_suffix('>'))
    {
        let inner_type = FieldType::parse(inner).to_python_type(None);
        return format!("Optional[{}]", inner_type);
    }

    if let Some(inner) = s_lower
        .strip_prefix("map<")
        .and_then(|s| s.strip_suffix('>'))
    {
        if let Some((key, value)) = inner.split_once(',') {
            let key_type = FieldType::parse(key.trim()).to_python_type(None);
            let value_type = FieldType::parse(value.trim()).to_python_type(None);
            return format!("dict[{}, {}]", key_type, value_type);
        }
    }

    "Any".to_string()
}

/// Parse complex type string for TypeScript.
fn parse_complex_typescript_type(s: &str, nested_interface_name: Option<&str>) -> String {
    let s_lower = s.to_lowercase();

    if s_lower == "object" {
        return nested_interface_name
            .unwrap_or("Record<string, unknown>")
            .to_string();
    }

    if let Some(inner) = s_lower
        .strip_prefix("array<")
        .and_then(|s| s.strip_suffix('>'))
    {
        let inner_type = FieldType::parse(inner).to_typescript_type(None);
        return format!("{}[]", inner_type);
    }

    if let Some(inner) = s_lower
        .strip_prefix("optional<")
        .and_then(|s| s.strip_suffix('>'))
    {
        let inner_type = FieldType::parse(inner).to_typescript_type(None);
        return format!("{} | null", inner_type);
    }

    if let Some(inner) = s_lower
        .strip_prefix("map<")
        .and_then(|s| s.strip_suffix('>'))
    {
        if let Some((key, value)) = inner.split_once(',') {
            let key_type = FieldType::parse(key.trim()).to_typescript_type(None);
            let value_type = FieldType::parse(value.trim()).to_typescript_type(None);
            return format!("Record<{}, {}>", key_type, value_type);
        }
    }

    "unknown".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_id_computation() {
        let id1 = compute_schema_id("com.tatolab.videoframe@1.0.0");
        let id2 = compute_schema_id("com.tatolab.videoframe@1.0.0");
        let id3 = compute_schema_id("com.tatolab.videoframe@2.0.0");

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_rust_struct_name() {
        let schema = SchemaDefinition {
            name: "com.tatolab.videoframe".to_string(),
            version: "1.0.0".to_string(),
            description: None,
            fields: vec![],
        };

        assert_eq!(schema.rust_struct_name(), "Videoframe");
    }

    #[test]
    fn test_field_type_rust() {
        assert_eq!(FieldType::String.to_rust_type(None), "String");
        assert_eq!(FieldType::Uint32.to_rust_type(None), "u32");
        assert_eq!(FieldType::Float64.to_rust_type(None), "f64");
        assert_eq!(FieldType::Bytes.to_rust_type(None), "Vec<u8>");

        assert_eq!(
            FieldType::parse("array<uint8>").to_rust_type(None),
            "Vec<u8>"
        );
        assert_eq!(
            FieldType::parse("optional<string>").to_rust_type(None),
            "Option<String>"
        );
        assert_eq!(
            FieldType::parse("map<string,int32>").to_rust_type(None),
            "std::collections::HashMap<String, i32>"
        );
    }

    #[test]
    fn test_to_pascal_case() {
        assert_eq!(to_pascal_case("video_frame"), "VideoFrame");
        assert_eq!(to_pascal_case("videoframe"), "Videoframe");
        assert_eq!(to_pascal_case("video-frame"), "VideoFrame");
    }

    #[test]
    fn test_to_snake_case() {
        assert_eq!(to_snake_case("VideoFrame"), "video_frame");
        assert_eq!(to_snake_case("videoFrame"), "video_frame");
        // Consecutive uppercase treated as single word
        assert_eq!(to_snake_case("HTTPServer"), "httpserver");
    }
}
