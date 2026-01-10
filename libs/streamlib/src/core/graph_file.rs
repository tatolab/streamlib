// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Declarative graph file format for loading pipelines from JSON/YAML.
//!
//! This module defines the schema for graph files that can be loaded by
//! `streamlib-cli` or programmatically via [`StreamRuntime::load_graph_file`].
//!
//! # Example Graph File
//!
//! ```json
//! {
//!   "name": "camera-display",
//!   "processors": [
//!     { "alias": "camera", "type": "CameraProcessor", "config": {} },
//!     { "alias": "display", "type": "DisplayProcessor", "config": { "width": 1920, "height": 1080 } }
//!   ],
//!   "connections": [
//!     { "from": "camera.video", "to": "display.video" }
//!   ]
//! }
//! ```

use serde::{Deserialize, Serialize};

use crate::core::{ProcessorSpec, Result, StreamError};

/// Declarative graph definition loaded from JSON/YAML files.
///
/// Processors are identified by local aliases within the file. These aliases
/// are resolved to runtime-generated processor IDs during loading.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphFileDefinition {
    /// Optional pipeline name for display/logging.
    #[serde(default)]
    pub name: Option<String>,

    /// Processor definitions with local aliases.
    pub processors: Vec<ProcessorDefinition>,

    /// Connections between processors using aliases.
    #[serde(default)]
    pub connections: Vec<ConnectionDefinition>,
}

/// A processor definition in the graph file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessorDefinition {
    /// Local alias for referencing in connections.
    ///
    /// Must be unique within the graph file. Used in connection definitions
    /// as `"alias.port_name"`.
    pub alias: String,

    /// Processor type name (must exist in PROCESSOR_REGISTRY).
    ///
    /// Examples: "CameraProcessor", "DisplayProcessor", "PythonContinuousHostProcessor"
    #[serde(rename = "type")]
    pub processor_type: String,

    /// Processor configuration as JSON.
    ///
    /// Must match the config schema for the processor type.
    #[serde(default)]
    pub config: serde_json::Value,
}

/// A connection definition using aliases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionDefinition {
    /// Source output port: `"alias.port_name"`
    pub from: String,

    /// Target input port: `"alias.port_name"`
    pub to: String,
}

/// Parsed port reference with alias and port name.
#[derive(Debug, Clone)]
pub struct ParsedPortRef<'a> {
    pub alias: &'a str,
    pub port_name: &'a str,
}

impl ConnectionDefinition {
    /// Parse the `from` field into alias and port name.
    pub fn parse_from(&self) -> Result<ParsedPortRef<'_>> {
        parse_port_ref(&self.from)
    }

    /// Parse the `to` field into alias and port name.
    pub fn parse_to(&self) -> Result<ParsedPortRef<'_>> {
        parse_port_ref(&self.to)
    }
}

/// Parse `"alias.port_name"` into components.
fn parse_port_ref(s: &str) -> Result<ParsedPortRef<'_>> {
    let parts: Vec<&str> = s.splitn(2, '.').collect();
    if parts.len() != 2 {
        return Err(StreamError::GraphError(format!(
            "Invalid port reference '{}', expected 'alias.port_name'",
            s
        )));
    }
    Ok(ParsedPortRef {
        alias: parts[0],
        port_name: parts[1],
    })
}

impl ProcessorDefinition {
    /// Convert to a ProcessorSpec for runtime instantiation.
    pub fn to_processor_spec(&self) -> ProcessorSpec {
        ProcessorSpec::new(&self.processor_type, self.config.clone())
    }
}

impl GraphFileDefinition {
    /// Load from a JSON file path.
    pub fn from_json_file(path: &std::path::Path) -> Result<Self> {
        let file = std::fs::File::open(path).map_err(|e| {
            StreamError::GraphError(format!(
                "Failed to open graph file '{}': {}",
                path.display(),
                e
            ))
        })?;

        serde_json::from_reader(file).map_err(|e| {
            StreamError::GraphError(format!(
                "Failed to parse graph file '{}': {}",
                path.display(),
                e
            ))
        })
    }

    /// Load from a JSON string.
    pub fn from_json_str(json: &str) -> Result<Self> {
        serde_json::from_str(json)
            .map_err(|e| StreamError::GraphError(format!("Failed to parse graph JSON: {}", e)))
    }

    /// Validate the graph definition without loading it.
    ///
    /// Checks:
    /// - All aliases are unique
    /// - All connection references point to valid aliases
    /// - All processor types exist in registry (optional, requires registry access)
    pub fn validate(&self) -> Result<()> {
        use std::collections::HashSet;

        // Check for duplicate aliases
        let mut aliases: HashSet<&str> = HashSet::new();
        for proc in &self.processors {
            if !aliases.insert(proc.alias.as_str()) {
                return Err(StreamError::GraphError(format!(
                    "Duplicate processor alias: '{}'",
                    proc.alias
                )));
            }
        }

        // Check all connection references
        for conn in &self.connections {
            let from = conn.parse_from()?;
            let to = conn.parse_to()?;

            if !aliases.contains(&from.alias) {
                return Err(StreamError::GraphError(format!(
                    "Connection references unknown processor alias: '{}'",
                    from.alias
                )));
            }
            if !aliases.contains(&to.alias) {
                return Err(StreamError::GraphError(format!(
                    "Connection references unknown processor alias: '{}'",
                    to.alias
                )));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_graph() {
        let json = r#"{
            "name": "test-pipeline",
            "processors": [
                { "alias": "camera", "type": "CameraProcessor", "config": {} },
                { "alias": "display", "type": "DisplayProcessor", "config": { "width": 1920 } }
            ],
            "connections": [
                { "from": "camera.video", "to": "display.video" }
            ]
        }"#;

        let def = GraphFileDefinition::from_json_str(json).unwrap();

        assert_eq!(def.name, Some("test-pipeline".to_string()));
        assert_eq!(def.processors.len(), 2);
        assert_eq!(def.processors[0].alias, "camera");
        assert_eq!(def.processors[0].processor_type, "CameraProcessor");
        assert_eq!(def.processors[1].alias, "display");
        assert_eq!(def.connections.len(), 1);
        assert_eq!(def.connections[0].from, "camera.video");
        assert_eq!(def.connections[0].to, "display.video");
    }

    #[test]
    fn test_parse_port_ref() {
        let result = parse_port_ref("camera.video").unwrap();
        assert_eq!(result.alias, "camera");
        assert_eq!(result.port_name, "video");

        let result = parse_port_ref("my_processor.video_out").unwrap();
        assert_eq!(result.alias, "my_processor");
        assert_eq!(result.port_name, "video_out");
    }

    #[test]
    fn test_parse_port_ref_invalid() {
        assert!(parse_port_ref("no_dot").is_err());
        assert!(parse_port_ref("").is_err());
    }

    #[test]
    fn test_validate_duplicate_alias() {
        let json = r#"{
            "processors": [
                { "alias": "cam", "type": "CameraProcessor", "config": {} },
                { "alias": "cam", "type": "DisplayProcessor", "config": {} }
            ]
        }"#;

        let def = GraphFileDefinition::from_json_str(json).unwrap();
        assert!(def.validate().is_err());
    }

    #[test]
    fn test_validate_unknown_alias_in_connection() {
        let json = r#"{
            "processors": [
                { "alias": "camera", "type": "CameraProcessor", "config": {} }
            ],
            "connections": [
                { "from": "camera.video", "to": "unknown.video" }
            ]
        }"#;

        let def = GraphFileDefinition::from_json_str(json).unwrap();
        assert!(def.validate().is_err());
    }

    #[test]
    fn test_minimal_graph() {
        let json = r#"{ "processors": [] }"#;
        let def = GraphFileDefinition::from_json_str(json).unwrap();
        assert!(def.name.is_none());
        assert!(def.processors.is_empty());
        assert!(def.connections.is_empty());
        assert!(def.validate().is_ok());
    }
}
