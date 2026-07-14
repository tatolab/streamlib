// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Declarative graph snapshot format for round-tripping a runtime's
//! graph through JSON.
//!
//! A snapshot is the typed wire shape behind
//! [`Runner::load_graph_snapshot`](crate::core::runtime::Runner::load_graph_snapshot)
//! and
//! [`Runner::save_graph_snapshot`](crate::core::runtime::Runner::save_graph_snapshot):
//! save walks the live graph and emits one of these, load applies it
//! to an empty runtime. The shape is symmetric — load(save(g)) yields
//! a graph that, when saved, byte-equals the first save.
//!
//! # Example snapshot
//!
//! ```json
//! {
//!   "name": "camera-display",
//!   "processors": [
//!     {
//!       "alias": "camera",
//!       "type": { "org": "tatolab", "package": "streamlib",
//!                 "type": "CameraProcessor", "version": "1.0.0" },
//!       "config": {}
//!     },
//!     {
//!       "alias": "display",
//!       "type": { "org": "tatolab", "package": "streamlib",
//!                 "type": "DisplayProcessor", "version": "1.0.0" },
//!       "config": { "width": 1920, "height": 1080 }
//!     }
//!   ],
//!   "connections": [
//!     { "from": "camera.video", "to": "display.video" }
//!   ]
//! }
//! ```

use serde::{Deserialize, Serialize};

use crate::core::descriptors::SchemaIdent;
use crate::core::{Error, ProcessorSpec, Result};

/// Round-trippable JSON shape for a runtime's graph.
///
/// Processors are identified by local aliases within the snapshot.
/// These aliases are resolved to runtime-generated processor IDs on
/// load and regenerated deterministically on save.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphSnapshot {
    /// Optional pipeline name for display/logging.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Processor definitions with local aliases.
    pub processors: Vec<ProcessorDefinition>,

    /// Connections between processors using aliases.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connections: Vec<ConnectionDefinition>,
}

/// A processor definition in the snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessorDefinition {
    /// Local alias for referencing in connections.
    ///
    /// Must be unique within the snapshot. Used in connection
    /// definitions as `"alias.port_name"`.
    pub alias: String,

    /// Structured processor identity — `@org/package/Type@version`
    /// rendered as four typed fields. The structured-everywhere rule
    /// applies on the snapshot wire format too — bare strings like
    /// `"CameraProcessor"` are rejected at deserialize time (no
    /// parser shim).
    #[serde(rename = "type")]
    pub processor_type: SchemaIdent,

    /// Processor configuration as JSON.
    ///
    /// Must match the config schema for the processor type.
    #[serde(default)]
    pub config: serde_json::Value,

    /// Optional display-name override. Absent ↔ default to the
    /// processor's PascalCase short name. Save side omits this field
    /// when the live node's display name equals the auto-default, so
    /// the user-intent distinction round-trips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// A connection definition using aliases.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
        return Err(Error::GraphError(format!(
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
    /// Convert to a [`ProcessorSpec`] for runtime instantiation.
    pub fn to_processor_spec(&self) -> ProcessorSpec {
        let mut spec = ProcessorSpec::new(self.processor_type.clone(), self.config.clone());
        if let Some(name) = &self.display_name {
            spec = spec.with_display_name(name.clone());
        }
        spec
    }
}

impl GraphSnapshot {
    /// Load a snapshot from a JSON file path.
    pub fn from_json_file(path: &std::path::Path) -> Result<Self> {
        let file = std::fs::File::open(path).map_err(|e| {
            Error::GraphError(format!(
                "Failed to open snapshot file '{}': {}",
                path.display(),
                e
            ))
        })?;

        serde_json::from_reader(file).map_err(|e| {
            Error::GraphError(format!(
                "Failed to parse snapshot file '{}': {}",
                path.display(),
                e
            ))
        })
    }

    /// Load a snapshot from a JSON string.
    pub fn from_json_str(json: &str) -> Result<Self> {
        serde_json::from_str(json)
            .map_err(|e| Error::GraphError(format!("Failed to parse snapshot JSON: {}", e)))
    }

    /// Serialize this snapshot as a pretty-printed JSON string.
    pub fn to_json_string(&self) -> Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| Error::GraphError(format!("Failed to serialize snapshot: {}", e)))
    }

    /// Serialize this snapshot to a JSON file path.
    pub fn to_json_file(&self, path: &std::path::Path) -> Result<()> {
        let body = self.to_json_string()?;
        std::fs::write(path, body).map_err(|e| {
            Error::GraphError(format!(
                "Failed to write snapshot file '{}': {}",
                path.display(),
                e
            ))
        })
    }

    /// Validate the snapshot without loading it.
    ///
    /// Checks:
    /// - All aliases are unique
    /// - All connection references point to valid aliases
    /// - All processor types exist in the global processor registry
    pub fn validate(&self) -> Result<()> {
        use std::collections::HashSet;

        use crate::core::processors::PROCESSOR_REGISTRY;

        // Check for duplicate aliases
        let mut aliases: HashSet<&str> = HashSet::new();
        for proc in &self.processors {
            if !aliases.insert(proc.alias.as_str()) {
                return Err(Error::GraphError(format!(
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
                return Err(Error::GraphError(format!(
                    "Connection references unknown processor alias: '{}'",
                    from.alias
                )));
            }
            if !aliases.contains(&to.alias) {
                return Err(Error::GraphError(format!(
                    "Connection references unknown processor alias: '{}'",
                    to.alias
                )));
            }
        }

        // Check all processor types resolve through the runtime registry.
        // Bails on the first miss — matches the behavior of the
        // surrounding alias / connection checks above.
        for proc in &self.processors {
            if PROCESSOR_REGISTRY.port_info(&proc.processor_type).is_none() {
                return Err(Error::UnknownProcessorType {
                    ident: proc.processor_type.clone(),
                });
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper for tests — a structured 4-field type literal at @tatolab/streamlib.
    /// `SemVer` deserializes from the dotted string form `"1.0.0"`, not a
    /// `{major, minor, patch}` object — see `streamlib-idents::semver`.
    fn structured_type(short: &str) -> String {
        format!(
            r#"{{ "org": "tatolab", "package": "streamlib", "type": "{}", "version": "1.0.0" }}"#,
            short
        )
    }

    #[test]
    fn test_parse_simple_snapshot() {
        let json = format!(
            r#"{{
                "name": "test-pipeline",
                "processors": [
                    {{ "alias": "camera", "type": {}, "config": {{}} }},
                    {{ "alias": "display", "type": {}, "config": {{ "width": 1920 }} }}
                ],
                "connections": [
                    {{ "from": "camera.video", "to": "display.video" }}
                ]
            }}"#,
            structured_type("CameraProcessor"),
            structured_type("DisplayProcessor"),
        );

        let snap = GraphSnapshot::from_json_str(&json).unwrap();

        assert_eq!(snap.name, Some("test-pipeline".to_string()));
        assert_eq!(snap.processors.len(), 2);
        assert_eq!(snap.processors[0].alias, "camera");
        assert_eq!(
            snap.processors[0].processor_type.r#type.as_str(),
            "CameraProcessor"
        );
        assert_eq!(snap.processors[0].processor_type.org.as_str(), "tatolab");
        assert!(snap.processors[0].display_name.is_none());
        assert_eq!(snap.processors[1].alias, "display");
        assert_eq!(snap.connections.len(), 1);
        assert_eq!(snap.connections[0].from, "camera.video");
        assert_eq!(snap.connections[0].to, "display.video");
    }

    #[test]
    fn test_round_trip_serde_preserves_structured_processor_type() {
        let json = format!(
            r#"{{
                "processors": [
                    {{ "alias": "camera", "type": {}, "config": {{}} }}
                ]
            }}"#,
            structured_type("CameraProcessor"),
        );
        let snap = GraphSnapshot::from_json_str(&json).unwrap();
        let back = serde_json::to_value(&snap).unwrap();
        let proc_type = &back["processors"][0]["type"];
        assert!(
            proc_type.is_object(),
            "processor_type must round-trip as a structured object, not a string"
        );
        assert_eq!(proc_type["org"], "tatolab");
        assert_eq!(proc_type["package"], "streamlib");
        assert_eq!(proc_type["type"], "CameraProcessor");
        // `SemVer` serializes as the dotted string form, not a structured
        // {major, minor, patch} object — see `streamlib-idents::semver`.
        assert_eq!(proc_type["version"], "1.0.0");
    }

    #[test]
    fn test_bare_string_processor_type_is_rejected() {
        // Pre-1.0 forbids parser shims — a bare string `"CameraProcessor"`
        // for the type field must fail to deserialize.
        let json = r#"{
            "processors": [
                { "alias": "camera", "type": "CameraProcessor", "config": {} }
            ]
        }"#;
        let res = GraphSnapshot::from_json_str(json);
        assert!(res.is_err(), "bare string processor_type must be rejected");
    }

    #[test]
    fn test_display_name_optional_field_round_trips() {
        let json = format!(
            r#"{{
                "processors": [
                    {{ "alias": "cam_a", "type": {}, "config": {{}},
                       "display_name": "Camera A" }}
                ]
            }}"#,
            structured_type("CameraProcessor"),
        );
        let snap = GraphSnapshot::from_json_str(&json).unwrap();
        assert_eq!(snap.processors[0].display_name.as_deref(), Some("Camera A"));
        let spec = snap.processors[0].to_processor_spec();
        assert_eq!(spec.display_name.as_deref(), Some("Camera A"));

        // Re-serialize and confirm the field survives.
        let back: serde_json::Value = serde_json::to_value(&snap).unwrap();
        assert_eq!(back["processors"][0]["display_name"], "Camera A");
    }

    #[test]
    fn test_to_json_string_round_trip() {
        let json_in = format!(
            r#"{{
                "name": "rt",
                "processors": [
                    {{ "alias": "camera", "type": {}, "config": {{}} }}
                ],
                "connections": []
            }}"#,
            structured_type("CameraProcessor"),
        );
        let snap = GraphSnapshot::from_json_str(&json_in).unwrap();
        let json_out = snap.to_json_string().unwrap();
        let snap_back = GraphSnapshot::from_json_str(&json_out).unwrap();
        assert_eq!(snap, snap_back);
    }

    #[test]
    fn test_to_json_file_round_trip() {
        let json_in = format!(
            r#"{{
                "processors": [
                    {{ "alias": "camera", "type": {}, "config": {{ "n": 7 }} }}
                ]
            }}"#,
            structured_type("CameraProcessor"),
        );
        let snap = GraphSnapshot::from_json_str(&json_in).unwrap();
        let tmp = std::env::temp_dir().join(format!(
            "streamlib-graph-snapshot-test-{}.json",
            std::process::id()
        ));
        snap.to_json_file(&tmp).unwrap();
        let snap_back = GraphSnapshot::from_json_file(&tmp).unwrap();
        std::fs::remove_file(&tmp).ok();
        assert_eq!(snap, snap_back);
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
        let json = format!(
            r#"{{
                "processors": [
                    {{ "alias": "cam", "type": {}, "config": {{}} }},
                    {{ "alias": "cam", "type": {}, "config": {{}} }}
                ]
            }}"#,
            structured_type("CameraProcessor"),
            structured_type("DisplayProcessor"),
        );

        let snap = GraphSnapshot::from_json_str(&json).unwrap();
        assert!(snap.validate().is_err());
    }

    #[test]
    fn test_validate_unknown_alias_in_connection() {
        let json = format!(
            r#"{{
                "processors": [
                    {{ "alias": "camera", "type": {}, "config": {{}} }}
                ],
                "connections": [
                    {{ "from": "camera.video", "to": "unknown.video" }}
                ]
            }}"#,
            structured_type("CameraProcessor"),
        );

        let snap = GraphSnapshot::from_json_str(&json).unwrap();
        assert!(snap.validate().is_err());
    }

    #[test]
    fn test_minimal_snapshot() {
        let json = r#"{ "processors": [] }"#;
        let snap = GraphSnapshot::from_json_str(json).unwrap();
        assert!(snap.name.is_none());
        assert!(snap.processors.is_empty());
        assert!(snap.connections.is_empty());
        assert!(snap.validate().is_ok());
    }

    /// `validate()` checks every processor type against the global registry
    /// and fails with the typed `UnknownProcessorType` variant on the first
    /// miss. The docstring promised this; the implementation now delivers.
    #[test]
    fn test_validate_unknown_processor_type() {
        let unknown_type = format!(
            r#"{{ "org": "tatolab", "package": "streamlib", "type": "{}", "version": "1.0.0" }}"#,
            "DefinitelyNotARegisteredProcessor",
        );
        let json = format!(
            r#"{{
                "processors": [
                    {{ "alias": "ghost", "type": {}, "config": {{}} }}
                ]
            }}"#,
            unknown_type,
        );

        let snap = GraphSnapshot::from_json_str(&json).unwrap();
        match snap.validate() {
            Err(Error::UnknownProcessorType { ident }) => {
                assert_eq!(ident.r#type.as_str(), "DefinitelyNotARegisteredProcessor");
            }
            other => panic!("expected UnknownProcessorType, got {:?}", other),
        }
    }
}
