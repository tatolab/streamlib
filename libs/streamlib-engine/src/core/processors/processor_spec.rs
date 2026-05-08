// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};

use crate::core::descriptors::SchemaIdent;

/// Specification for creating a processor.
///
/// Contains only what the user provides: processor identity and configuration.
/// Internal details (id, ports) are resolved by the runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessorSpec {
    /// Structured processor identity (matches the registered key in [`PROCESSOR_REGISTRY`](crate::core::processors::PROCESSOR_REGISTRY)).
    pub name: SchemaIdent,
    /// Configuration as JSON value.
    pub config: serde_json::Value,
    /// Display name override. If `None`, defaults to the processor's PascalCase short name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

impl ProcessorSpec {
    pub fn new(name: SchemaIdent, config: serde_json::Value) -> Self {
        Self {
            name,
            config,
            display_name: None,
        }
    }

    /// Set a custom display name for this processor.
    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        self.display_name = Some(display_name.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::descriptors::{Org, Package, SemVer, TypeName};

    fn ident(org: &str, pkg: &str, ty: &str, v: SemVer) -> SchemaIdent {
        SchemaIdent::new(
            Org::new(org).unwrap(),
            Package::new(pkg).unwrap(),
            TypeName::new(ty).unwrap(),
            v,
        )
    }

    #[test]
    fn serde_round_trip_preserves_structured_identity() {
        let spec = ProcessorSpec::new(
            ident("tatolab", "streamlib", "CameraProcessor", SemVer::new(1, 0, 0)),
            serde_json::json!({"width": 1920, "height": 1080}),
        );
        let json = serde_json::to_string(&spec).unwrap();
        let back: ProcessorSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec.name, back.name);
        assert_eq!(spec.config, back.config);
        assert_eq!(spec.display_name, back.display_name);
    }

    #[test]
    fn serde_emits_structured_name_object_not_joined_string() {
        // Wire-format lock — the name field on the wire is a structured 4-key
        // object, not the joined `@org/pkg/Type@version` form. The structured
        // shape is the "structured-everywhere" rule from the architecture
        // preamble (notes 1, 2 of the issue body).
        let spec = ProcessorSpec::new(
            ident("tatolab", "core", "VideoFrame", SemVer::new(1, 0, 0)),
            serde_json::Value::Null,
        );
        let json: serde_json::Value = serde_json::to_value(&spec).unwrap();
        let name = &json["name"];
        assert!(
            name.is_object(),
            "name must be a structured JSON object, not a string"
        );
        assert_eq!(name["org"], "tatolab");
        assert_eq!(name["package"], "core");
        assert_eq!(name["type"], "VideoFrame");
        // `SemVer` serializes as the dotted string form `"1.0.0"`,
        // not a structured `{major, minor, patch}` object — see
        // `streamlib-idents::semver`. The four-field rule applies to
        // `SchemaIdent` segments (org/package/type/version), not to the
        // version's internal representation.
        assert_eq!(name["version"], "1.0.0");
    }

    #[test]
    fn deserialize_rejects_bare_string_name() {
        // Pre-1.0 forbids parser shims — a bare string like `"CameraProcessor"`
        // for the name field must fail to deserialize.
        let json = r#"{"name":"CameraProcessor","config":null}"#;
        let res: Result<ProcessorSpec, _> = serde_json::from_str(json);
        assert!(res.is_err(), "bare string name must be rejected");
    }

    #[test]
    fn with_display_name_overrides_default() {
        let spec = ProcessorSpec::new(
            ident("tatolab", "core", "VideoFrame", SemVer::new(1, 0, 0)),
            serde_json::Value::Null,
        )
        .with_display_name("Camera A");
        assert_eq!(spec.display_name.as_deref(), Some("Camera A"));
    }
}
