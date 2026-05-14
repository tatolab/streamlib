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
    // Name must be a bare PascalCase short name (`Camera`, `BlurFilter`)
    // matching the macro's contract — `(org, package)` come from the
    // enclosing `streamlib.yaml`'s `package:` block, not from the
    // processor schema itself. Legacy reverse-DNS shapes
    // (`com.example.foo`) are rejected at parse time.
    if schema.name.is_empty() {
        return Err(SchemaError::MissingField {
            field: "name".to_string(),
        });
    }
    if streamlib_idents::TypeName::new(schema.name.as_str()).is_err() {
        return Err(SchemaError::InvalidName {
            name: schema.name.clone(),
            reason: "processor `name:` must be a bare PascalCase short name \
                     (`^[A-Z][A-Za-z0-9]*$`) — the `(org, package)` come from \
                     the enclosing `streamlib.yaml`'s `package:` block. Legacy \
                     reverse-DNS shapes (`com.example.foo`) are no longer \
                     accepted; see docs/architecture/schema-identity-and-packaging.md."
                .to_string(),
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

    // Config schema reference: shape (non-empty bare PascalCase TypeName)
    // is locked by `TypeName`'s typed deserializer in
    // `streamlib_idents::TypeName`. Resolution against the enclosing
    // manifest's `schemas:` map happens downstream (proc-macro expansion /
    // runtime startup); the standalone parser doesn't have package context.

    // Validate input port schema references — port name presence + buffer
    // size sanity. Schema shape is locked by [`PortSchemaSpec`]'s typed
    // deserializer (rejects joined-string and other non-structured forms).
    for input in &schema.inputs {
        if input.name.is_empty() {
            return Err(SchemaError::InvalidName {
                name: schema.name.clone(),
                reason: "input port name cannot be empty".to_string(),
            });
        }
        if input.buffer_size == Some(0) {
            return Err(SchemaError::InvalidName {
                name: schema.name.clone(),
                reason: format!(
                    "input '{}' buffer_size cannot be 0",
                    input.name
                ),
            });
        }
    }

    // Validate output port schema references — port name presence only.
    for output in &schema.outputs {
        if output.name.is_empty() {
            return Err(SchemaError::InvalidName {
                name: schema.name.clone(),
                reason: "output port name cannot be empty".to_string(),
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
name: Passthrough
version: 1.0.0
"#;

        let schema = parse_processor_yaml(yaml).unwrap();
        assert_eq!(schema.name, "Passthrough");
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
name: Blur
version: 1.0.0
description: "Gaussian blur filter"

runtime: rust
entrypoint: src.blur:BlurProcessor

config:
  name: config
  schema: BlurConfig

inputs:
  - name: image_in
    schema: Frame
    description: "Input video frame"

outputs:
  - name: image_out
    schema: Frame
    description: "Blurred video frame"
"#;

        let schema = parse_processor_yaml(yaml).unwrap();
        assert_eq!(schema.name, "Blur");
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
        assert_eq!(config.schema.as_str(), "BlurConfig");

        assert_eq!(schema.inputs.len(), 1);
        assert_eq!(schema.inputs[0].name, "image_in");
        assert_eq!(schema.inputs[0].schema.to_string(), "Frame");

        assert_eq!(schema.outputs.len(), 1);
        assert_eq!(schema.outputs[0].name, "image_out");
    }

    #[test]
    fn test_parse_processor_schema_python_runtime() {
        let yaml = r#"
name: ObjectDetector
version: 1.0.0
runtime: python
entrypoint: detector:ObjectDetector

inputs:
  - name: frame
    schema: Frame

outputs:
  - name: detections
    schema: Detections
"#;

        let schema = parse_processor_yaml(yaml).unwrap();
        assert_eq!(
            schema.runtime.language,
            crate::processor_schema::ProcessorLanguage::Python
        );
    }

    #[test]
    fn test_processor_schema_accepts_pascal_case_short_name() {
        // The CLI validator requires PascalCase short names; the
        // `(org, package)` come from the enclosing `streamlib.yaml`.
        let yaml = r#"
name: Camera
version: 1.0.0
"#;

        let schema = parse_processor_yaml(yaml).unwrap();
        assert_eq!(schema.name, "Camera");
    }

    #[test]
    fn test_processor_schema_rejects_reverse_dns_name() {
        // Legacy reverse-DNS shapes are no longer accepted on the
        // `name:` field — the macro contract requires a bare
        // PascalCase short name.
        let yaml = r#"
name: com.example.legacy_processor
version: 1.0.0
"#;

        let result = parse_processor_yaml(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("PascalCase") || err.contains("bare"),
            "expected PascalCase guidance, got: {err}"
        );
    }

    #[test]
    fn test_processor_schema_rejects_snake_case_name() {
        let yaml = r#"
name: blur_filter
version: 1.0.0
"#;

        let result = parse_processor_yaml(yaml);
        assert!(result.is_err());
    }

    #[test]
    fn test_processor_schema_rejects_empty_name() {
        let yaml = r#"
name: ""
version: 1.0.0
"#;

        let result = parse_processor_yaml(yaml);
        assert!(result.is_err());
    }

    #[test]
    fn test_processor_schema_invalid_version() {
        let yaml = r#"
name: Test
version: invalid
"#;

        let result = parse_processor_yaml(yaml);
        assert!(result.is_err());
    }

    #[test]
    fn test_processor_schema_rejects_joined_string_port_schema() {
        // Joined-string `com.streamlib.video.frame` is not a valid bare
        // PascalCase TypeName — `PortSchemaSpec` only accepts `any` or a
        // bare PascalCase TypeName resolved against the manifest's
        // `schemas:` map.
        let yaml = r#"
name: Test
version: 1.0.0

inputs:
  - name: video
    schema: com.streamlib.video.frame
"#;

        let result = parse_processor_yaml(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("bare PascalCase TypeName") || err.contains("schemas: map"),
            "expected bare-name guidance, got: {err}"
        );
    }

    #[test]
    fn test_processor_schema_rejects_structured_port_form() {
        let yaml = r#"
name: Test
version: 1.0.0
inputs:
  - name: video
    schema: { org: tatolab, package: core, type: VideoFrame, version: 1.0.0 }
"#;
        let result = parse_processor_yaml(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("bare PascalCase TypeName") || err.contains("schemas: map"),
            "expected bare-name guidance, got: {err}"
        );
    }

    #[test]
    fn test_processor_schema_accepts_any_port_schema() {
        // `any` is the wildcard for ports that accept arbitrary serialized
        // payloads (e.g. MoQ tracks).
        let yaml = r#"
name: Test
version: 1.0.0

inputs:
  - name: data
    schema: any
"#;

        let schema = parse_processor_yaml(yaml).unwrap();
        assert_eq!(schema.inputs[0].schema.to_string(), "any");
    }

    #[test]
    fn test_processor_schema_config_local_type() {
        let yaml = r#"
name: Test
version: 1.0.0

config:
  name: config
  schema: MyConfig
"#;

        let result = parse_processor_yaml(yaml);
        assert!(result.is_ok());
        let schema = result.unwrap();
        assert_eq!(schema.config.as_ref().unwrap().schema.as_str(), "MyConfig");
    }

    #[test]
    fn test_processor_schema_full_name() {
        let yaml = r#"
name: Blur
version: 1.0.0
"#;

        let schema = parse_processor_yaml(yaml).unwrap();
        assert_eq!(schema.full_name(), "Blur@1.0.0");
    }

    #[test]
    fn test_processor_schema_rust_struct_name_is_identity_for_pascal_case() {
        let yaml = r#"
name: BlurFilter
version: 1.0.0
"#;

        let schema = parse_processor_yaml(yaml).unwrap();
        assert_eq!(schema.rust_struct_name(), "BlurFilter");
    }

    #[test]
    fn test_input_port_read_mode_and_buffer_size() {
        let yaml = r#"
name: Decoder
version: 1.0.0

inputs:
  - name: encoded_video_in
    schema: EncodedVideoFrame
    read_mode: read_next_in_order
    buffer_size: 16
"#;

        let schema = parse_processor_yaml(yaml).unwrap();
        assert_eq!(schema.inputs.len(), 1);
        assert_eq!(
            schema.inputs[0].read_mode,
            Some("read_next_in_order".to_string())
        );
        assert_eq!(schema.inputs[0].buffer_size, Some(16));
    }

    #[test]
    fn test_input_port_defaults_without_read_mode_and_buffer_size() {
        let yaml = r#"
name: Passthrough
version: 1.0.0

inputs:
  - name: video
    schema: Frame
"#;

        let schema = parse_processor_yaml(yaml).unwrap();
        assert_eq!(schema.inputs.len(), 1);
        assert_eq!(schema.inputs[0].read_mode, None);
        assert_eq!(schema.inputs[0].buffer_size, None);
    }

    #[test]
    fn scheduling_block_round_trips_priority() {
        let yaml = r#"
name: Audio
version: 1.0.0

scheduling:
  priority: realtime
"#;
        let schema = parse_processor_yaml(yaml).unwrap();
        let scheduling = schema.scheduling.expect("scheduling block parsed");
        assert_eq!(scheduling.priority, crate::ThreadPriority::RealTime);
    }

    #[test]
    fn scheduling_block_absent_yields_none() {
        let yaml = r#"
name: Passthrough
version: 1.0.0
"#;
        let schema = parse_processor_yaml(yaml).unwrap();
        assert!(schema.scheduling.is_none());
    }

    #[test]
    fn scheduling_block_rejects_invalid_priority() {
        let yaml = r#"
name: Bogus
version: 1.0.0

scheduling:
  priority: NotAValidVariant
"#;
        let result = parse_processor_yaml(yaml);
        let err = result.expect_err("invalid priority variant must error").to_string();
        assert!(
            err.contains("priority")
                || err.contains("realtime")
                || err.contains("variant"),
            "expected diagnostic to mention `priority`, `realtime`, or `variant`; got: {err}"
        );
    }

    #[test]
    fn scheduling_block_rejects_unknown_field() {
        let yaml = r#"
name: Bogus
version: 1.0.0

scheduling:
  priority: high
  totally_unknown: 42
"#;
        let result = parse_processor_yaml(yaml);
        assert!(
            result.is_err(),
            "deny_unknown_fields on ProcessorScheduling must reject extra keys"
        );
    }

    #[test]
    fn thread_priority_accepts_legacy_pascal_case_aliases() {
        let yaml = r#"
name: Audio
version: 1.0.0

scheduling:
  priority: RealTime
"#;
        let schema = parse_processor_yaml(yaml).unwrap();
        let scheduling = schema.scheduling.expect("scheduling block parsed");
        assert_eq!(scheduling.priority, crate::ThreadPriority::RealTime);
    }

    #[test]
    fn test_input_port_buffer_size_zero_rejected() {
        let yaml = r#"
name: Test
version: 1.0.0

inputs:
  - name: video
    schema: Frame
    buffer_size: 0
"#;

        let result = parse_processor_yaml(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("buffer_size cannot be 0"));
    }
}
