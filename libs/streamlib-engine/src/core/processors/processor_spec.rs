// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};

use crate::core::descriptors::SchemaIdent;

/// Specification for creating a processor.
///
/// Contains only what the user provides: processor identity and configuration.
/// Internal details (id, ports) are resolved by the runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
            ident(
                "tatolab",
                "streamlib",
                "CameraProcessor",
                SemVer::new(1, 0, 0),
            ),
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

    /// msgpack `to_vec_named` → `from_slice` round-trip preserves full
    /// value equality. Mirrors the runtime-ops-shim encode path the
    /// plugin ABI takes when forwarding `Runtime::add_processor` calls
    /// from cdylib code to the host.
    #[test]
    fn msgpack_round_trip_preserves_full_value() {
        let spec = ProcessorSpec::new(
            ident(
                "tatolab",
                "streamlib",
                "CameraProcessor",
                SemVer::new(1, 2, 3),
            ),
            serde_json::json!({
                "width": 1920,
                "height": 1080,
                "nested": {"key": "value", "arr": [1, 2, 3]},
            }),
        )
        .with_display_name("Camera A");

        let bytes = rmp_serde::to_vec_named(&spec).expect("encode");
        let back: ProcessorSpec = rmp_serde::from_slice(&bytes).expect("decode");
        assert_eq!(spec, back);
    }

    /// Empty config + absent display_name still round-trips.
    #[test]
    fn msgpack_round_trip_minimal_spec() {
        let spec = ProcessorSpec::new(
            ident("tatolab", "core", "VideoFrame", SemVer::new(0, 1, 0)),
            serde_json::Value::Null,
        );
        let bytes = rmp_serde::to_vec_named(&spec).expect("encode");
        let back: ProcessorSpec = rmp_serde::from_slice(&bytes).expect("decode");
        assert_eq!(spec, back);
        assert!(back.display_name.is_none());
    }

    /// Unicode in display_name + config string values survives the
    /// msgpack wire.
    #[test]
    fn msgpack_round_trip_unicode_preserved() {
        let spec = ProcessorSpec::new(
            ident("tatolab", "core", "VideoFrame", SemVer::new(1, 0, 0)),
            serde_json::json!({"label": "カメラ — 中文 — emoji 🎥"}),
        )
        .with_display_name("こんにちは");

        let bytes = rmp_serde::to_vec_named(&spec).expect("encode");
        let back: ProcessorSpec = rmp_serde::from_slice(&bytes).expect("decode");
        assert_eq!(spec, back);
    }

    /// Documents what the `serde_json::Value` ↔ rmp_serde round-trip
    /// actually preserves for the integer-typing axis. The wire is
    /// stable IF the test author understands its quirks: positive
    /// integers fitting in `u64` stay numerically equal, negative
    /// integers round-trip via `i64`, and floats stay floats. Mixed
    /// numeric containers are preserved at the value level.
    #[test]
    fn config_value_msgpack_round_trip_integer_axis() {
        let cases = [
            ("zero", serde_json::json!(0)),
            ("small_positive", serde_json::json!(42u32)),
            ("max_u32", serde_json::json!(u32::MAX)),
            ("max_u64", serde_json::json!(u64::MAX)),
            ("negative_small", serde_json::json!(-42i64)),
            ("min_i64", serde_json::json!(i64::MIN)),
            ("float", serde_json::json!(1.5f64)),
            (
                "mixed_array",
                serde_json::json!([0i64, -1i64, u64::MAX, 1.5f64, "string"]),
            ),
            (
                "nested",
                serde_json::json!({
                    "negative": -1i64,
                    "huge": u64::MAX,
                    "float": std::f64::consts::PI,
                    "inner": {"flag": true, "null": null},
                }),
            ),
        ];
        for (name, payload) in cases {
            let spec = ProcessorSpec::new(
                ident("tatolab", "core", "T", SemVer::new(1, 0, 0)),
                payload.clone(),
            );
            let bytes = rmp_serde::to_vec_named(&spec).expect("encode");
            let back: ProcessorSpec = rmp_serde::from_slice(&bytes).expect("decode");
            assert_eq!(
                spec, back,
                "{} round-trip lost equality at the Value level",
                name
            );
        }
    }
}
