// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Schema definition types.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
