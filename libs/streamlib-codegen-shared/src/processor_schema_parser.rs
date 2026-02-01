// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::error::{SchemaError, SchemaResult};
use crate::processor_schema::ProcessorSchema;
use std::path::Path;

/// Parse a processor schema from a YAML string.
pub fn parse_processor_yaml(yaml: &str) -> SchemaResult<ProcessorSchema> {
    let schema: ProcessorSchema = serde_yaml::from_str(yaml)?;
    validate_processor_schema(&schema)?;
    Ok(schema)
}

/// Parse a processor schema from a YAML file.
pub fn parse_processor_yaml_file(path: &Path) -> SchemaResult<ProcessorSchema> {
    let yaml = std::fs::read_to_string(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            SchemaError::FileNotFound {
                path: path.display().to_string(),
            }
        } else {
            SchemaError::IoError(e)
        }
    })?;

    parse_processor_yaml(&yaml)
}

/// Validate a parsed processor schema.
fn validate_processor_schema(schema: &ProcessorSchema) -> SchemaResult<()> {
    // Validate name format (reverse domain notation)
    if schema.name.is_empty() {
        return Err(SchemaError::MissingField {
            field: "name".to_string(),
        });
    }

    if !schema.name.contains('.') {
        return Err(SchemaError::InvalidName {
            name: schema.name.clone(),
            reason: "must use reverse domain notation (e.g., com.example.myprocessor)".to_string(),
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

    // Validate config schema reference
    if let Some(config) = &schema.config {
        if config.schema.is_empty() {
            return Err(SchemaError::InvalidName {
                name: config.schema.clone(),
                reason: "config schema cannot be empty".to_string(),
            });
        }
    }

    // Validate input port schema references
    for input in &schema.inputs {
        if input.name.is_empty() {
            return Err(SchemaError::InvalidName {
                name: schema.name.clone(),
                reason: "input port name cannot be empty".to_string(),
            });
        }
        if !input.schema.contains('@') {
            return Err(SchemaError::InvalidName {
                name: input.schema.clone(),
                reason: format!(
                    "input '{}' schema must include version (e.g., com.example.frame@1.0.0)",
                    input.name
                ),
            });
        }
    }

    // Validate output port schema references
    for output in &schema.outputs {
        if output.name.is_empty() {
            return Err(SchemaError::InvalidName {
                name: schema.name.clone(),
                reason: "output port name cannot be empty".to_string(),
            });
        }
        if !output.schema.contains('@') {
            return Err(SchemaError::InvalidName {
                name: output.schema.clone(),
                reason: format!(
                    "output '{}' schema must include version (e.g., com.example.frame@1.0.0)",
                    output.name
                ),
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_processor_schema_minimal() {
        let yaml = r#"
name: com.example.passthrough
version: 1.0.0
"#;

        let schema = parse_processor_yaml(yaml).unwrap();
        assert_eq!(schema.name, "com.example.passthrough");
        assert_eq!(schema.version, "1.0.0");
        assert!(schema.description.is_none());
        assert!(schema.entrypoint.is_none());
        assert!(schema.config.is_none());
        assert!(schema.inputs.is_empty());
        assert!(schema.outputs.is_empty());
    }

    #[test]
    fn test_parse_processor_schema_full() {
        let yaml = r#"
name: com.example.blur
version: 1.0.0
description: "Gaussian blur filter"

runtime: rust
entrypoint: src.blur:BlurProcessor

config:
  name: config
  schema: com.example.blur.config@1.0.0

inputs:
  - name: image_in
    schema: com.streamlib.video.frame@1.0.0
    description: "Input video frame"

outputs:
  - name: image_out
    schema: com.streamlib.video.frame@1.0.0
    description: "Blurred video frame"
"#;

        let schema = parse_processor_yaml(yaml).unwrap();
        assert_eq!(schema.name, "com.example.blur");
        assert_eq!(schema.version, "1.0.0");
        assert_eq!(schema.description, Some("Gaussian blur filter".to_string()));
        assert_eq!(
            schema.runtime.language,
            crate::processor_schema::ProcessorLanguage::Rust
        );
        assert_eq!(
            schema.entrypoint,
            Some("src.blur:BlurProcessor".to_string())
        );

        let config = schema.config.as_ref().unwrap();
        assert_eq!(config.name, "config");
        assert_eq!(config.schema, "com.example.blur.config@1.0.0");

        assert_eq!(schema.inputs.len(), 1);
        assert_eq!(schema.inputs[0].name, "image_in");
        assert_eq!(schema.inputs[0].schema, "com.streamlib.video.frame@1.0.0");

        assert_eq!(schema.outputs.len(), 1);
        assert_eq!(schema.outputs[0].name, "image_out");
    }

    #[test]
    fn test_parse_processor_schema_python_runtime() {
        let yaml = r#"
name: com.example.detector
version: 1.0.0
runtime: python
entrypoint: detector:ObjectDetector

inputs:
  - name: frame
    schema: com.streamlib.video.frame@1.0.0

outputs:
  - name: detections
    schema: com.example.detections@1.0.0
"#;

        let schema = parse_processor_yaml(yaml).unwrap();
        assert_eq!(
            schema.runtime.language,
            crate::processor_schema::ProcessorLanguage::Python
        );
    }

    #[test]
    fn test_processor_schema_invalid_name() {
        let yaml = r#"
name: invalidname
version: 1.0.0
"#;

        let result = parse_processor_yaml(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("reverse domain notation"));
    }

    #[test]
    fn test_processor_schema_invalid_version() {
        let yaml = r#"
name: com.example.test
version: invalid
"#;

        let result = parse_processor_yaml(yaml);
        assert!(result.is_err());
    }

    #[test]
    fn test_processor_schema_input_missing_version() {
        let yaml = r#"
name: com.example.test
version: 1.0.0

inputs:
  - name: video
    schema: com.streamlib.video.frame
"#;

        let result = parse_processor_yaml(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must include version"));
    }

    #[test]
    fn test_processor_schema_config_local_type() {
        let yaml = r#"
name: com.example.test
version: 1.0.0

config:
  name: config
  schema: MyConfig
"#;

        let result = parse_processor_yaml(yaml);
        assert!(result.is_ok());
        let schema = result.unwrap();
        assert_eq!(schema.config.as_ref().unwrap().schema, "MyConfig");
    }

    #[test]
    fn test_processor_schema_full_name() {
        let yaml = r#"
name: com.example.blur
version: 1.0.0
"#;

        let schema = parse_processor_yaml(yaml).unwrap();
        assert_eq!(schema.full_name(), "com.example.blur@1.0.0");
    }

    #[test]
    fn test_processor_schema_rust_struct_name() {
        let yaml = r#"
name: com.example.blur_filter
version: 1.0.0
"#;

        let schema = parse_processor_yaml(yaml).unwrap();
        assert_eq!(schema.rust_struct_name(), "BlurFilter");
    }
}
