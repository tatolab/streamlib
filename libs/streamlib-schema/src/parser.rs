// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! YAML schema parsing.

use crate::definition::SchemaDefinition;
use crate::error::{Result, SchemaError};
use std::path::Path;

/// Parse a schema from a YAML string.
pub fn parse_yaml(yaml: &str) -> Result<SchemaDefinition> {
    let schema: SchemaDefinition = serde_yaml::from_str(yaml)?;
    validate_schema(&schema)?;
    Ok(schema)
}

/// Parse a schema from a YAML file.
pub fn parse_yaml_file(path: &Path) -> Result<SchemaDefinition> {
    let yaml = std::fs::read_to_string(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            SchemaError::FileNotFound {
                path: path.display().to_string(),
            }
        } else {
            SchemaError::IoError(e)
        }
    })?;

    parse_yaml(&yaml)
}

/// Validate a parsed schema.
fn validate_schema(schema: &SchemaDefinition) -> Result<()> {
    // Validate name format (reverse domain notation)
    if schema.name.is_empty() {
        return Err(SchemaError::MissingField {
            field: "name".to_string(),
        });
    }

    if !schema.name.contains('.') {
        return Err(SchemaError::InvalidName {
            name: schema.name.clone(),
            reason: "must use reverse domain notation (e.g., com.example.myschema)".to_string(),
        });
    }

    // Validate version format (semver-like)
    if schema.version.is_empty() {
        return Err(SchemaError::MissingField {
            field: "version".to_string(),
        });
    }

    let version_parts: Vec<&str> = schema.version.split('.').collect();
    if version_parts.len() < 2 || version_parts.len() > 3 {
        return Err(SchemaError::InvalidName {
            name: schema.version.clone(),
            reason: "version must be in format X.Y or X.Y.Z".to_string(),
        });
    }

    for part in &version_parts {
        if part.parse::<u32>().is_err() {
            return Err(SchemaError::InvalidName {
                name: schema.version.clone(),
                reason: "version parts must be numeric".to_string(),
            });
        }
    }

    // Validate fields
    validate_fields(&schema.fields, &schema.name)?;

    Ok(())
}

/// Validate fields recursively.
fn validate_fields(fields: &[crate::definition::Field], schema_name: &str) -> Result<()> {
    for field in fields {
        if field.name.is_empty() {
            return Err(SchemaError::InvalidName {
                name: schema_name.to_string(),
                reason: "field name cannot be empty".to_string(),
            });
        }

        // Recursively validate nested fields
        if !field.fields.is_empty() {
            validate_fields(&field.fields, schema_name)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_schema() {
        let yaml = r#"
name: com.tatolab.videoframe
version: 1.0.0
description: "Video frame with surface reference"

fields:
  - name: surface_id
    type: uint64
    description: "GPU surface identifier"
  - name: width
    type: uint32
  - name: height
    type: uint32
  - name: timestamp_ns
    type: int64
"#;

        let schema = parse_yaml(yaml).unwrap();
        assert_eq!(schema.name, "com.tatolab.videoframe");
        assert_eq!(schema.version, "1.0.0");
        assert_eq!(schema.fields.len(), 4);
        assert_eq!(schema.fields[0].name, "surface_id");
    }

    #[test]
    fn test_parse_nested_schema() {
        let yaml = r#"
name: com.example.detection
version: 1.0.0

fields:
  - name: label
    type: string
  - name: confidence
    type: float32
  - name: bounding_box
    type: object
    fields:
      - name: x
        type: uint32
      - name: y
        type: uint32
      - name: width
        type: uint32
      - name: height
        type: uint32
"#;

        let schema = parse_yaml(yaml).unwrap();
        assert_eq!(schema.fields.len(), 3);
        assert_eq!(schema.fields[2].name, "bounding_box");
        assert_eq!(schema.fields[2].fields.len(), 4);
    }

    #[test]
    fn test_parse_complex_types() {
        let yaml = r#"
name: com.example.complex
version: 1.0.0

fields:
  - name: tags
    type: array<string>
  - name: metadata
    type: map<string,string>
  - name: optional_value
    type: optional<int32>
  - name: data
    type: bytes
"#;

        let schema = parse_yaml(yaml).unwrap();
        assert_eq!(schema.fields.len(), 4);
    }

    #[test]
    fn test_invalid_name_format() {
        let yaml = r#"
name: invalidname
version: 1.0.0
"#;

        let result = parse_yaml(yaml);
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_version_format() {
        let yaml = r#"
name: com.example.test
version: invalid
"#;

        let result = parse_yaml(yaml);
        assert!(result.is_err());
    }

    #[test]
    fn test_full_name() {
        let yaml = r#"
name: com.tatolab.videoframe
version: 1.0.0
"#;

        let schema = parse_yaml(yaml).unwrap();
        assert_eq!(schema.full_name(), "com.tatolab.videoframe@1.0.0");
    }

    #[test]
    fn test_parse_builtin_videoframe_schema() {
        let schema_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("schemas")
            .join("com.tatolab.videoframe.yaml");

        let schema = parse_yaml_file(&schema_path).unwrap();
        assert_eq!(schema.name, "com.tatolab.videoframe");
        assert_eq!(schema.version, "1.0.0");
        assert_eq!(schema.fields.len(), 6);

        // Verify key fields
        assert_eq!(schema.fields[0].name, "surface_id");
        assert_eq!(schema.fields[3].name, "pixel_format");
    }

    #[test]
    fn test_parse_builtin_audioframe_schema() {
        let schema_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("schemas")
            .join("com.tatolab.audioframe.yaml");

        let schema = parse_yaml_file(&schema_path).unwrap();
        assert_eq!(schema.name, "com.tatolab.audioframe");
        assert_eq!(schema.version, "1.0.0");
        assert_eq!(schema.fields.len(), 5);

        // Verify key fields
        assert_eq!(schema.fields[0].name, "samples");
        assert_eq!(schema.fields[1].name, "channels");
    }

    #[test]
    fn test_parse_builtin_encodedvideoframe_schema() {
        let schema_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("schemas")
            .join("com.tatolab.encodedvideoframe.yaml");

        let schema = parse_yaml_file(&schema_path).unwrap();
        assert_eq!(schema.name, "com.tatolab.encodedvideoframe");
        assert_eq!(schema.version, "1.0.0");
        assert_eq!(schema.fields.len(), 4);

        // Verify key fields
        assert_eq!(schema.fields[0].name, "data");
        assert_eq!(schema.fields[2].name, "is_keyframe");
    }
}
