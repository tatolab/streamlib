// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};

use crate::core::descriptors::ProcessorClassImportPath;

/// Specification for creating a processor.
///
/// Contains only what the user provides: processor identity and configuration.
/// Internal details (id, ports) are resolved by the runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessorSpec {
    /// The import path of the class to instantiate — looked up in the registry
    /// verbatim.
    pub name: ProcessorClassImportPath,
    /// Configuration as JSON value.
    pub config: serde_json::Value,
    /// Display name override. If `None`, defaults to the processor's PascalCase short name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

impl ProcessorSpec {
    /// Build a spec naming the class to instantiate by its import path.
    pub fn new(name: ProcessorClassImportPath, config: serde_json::Value) -> Self {
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

    fn class_import_path(path: &str) -> ProcessorClassImportPath {
        ProcessorClassImportPath::new(path).expect("the fixture path names a class")
    }

    /// Wire-format lock — the class a spec names is a plain string on the
    /// wire, carrying no org, package or version key, because there is no
    /// longer anything for a reader to key on but the path itself.
    #[test]
    fn serde_emits_the_class_import_path_as_a_plain_string() {
        let spec = ProcessorSpec::new(
            class_import_path("my_app.filters:BlurProcessor"),
            serde_json::Value::Null,
        );
        let json: serde_json::Value = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            json["name"],
            serde_json::Value::String("my_app.filters:BlurProcessor".to_string())
        );
    }

    /// Pre-1.0 forbids parser shims: the three-key `{org, package, type}`
    /// object the wire used to carry must fail to deserialize rather than be
    /// accepted alongside the string.
    #[test]
    fn deserialize_refuses_the_structured_object_the_wire_used_to_carry() {
        let json = r#"{"name":{"org":"tatolab","package":"core","type":"Camera"},"config":null}"#;
        let refused: Result<ProcessorSpec, _> = serde_json::from_str(json);
        assert!(
            refused.is_err(),
            "the old three-key object must not deserialize — there is no back-compat shape"
        );
    }

    #[test]
    fn deserialize_refuses_a_name_that_names_no_class() {
        let refused: Result<ProcessorSpec, _> =
            serde_json::from_str(r#"{"name":"","config":null}"#);
        assert!(
            refused.is_err(),
            "an empty class path must not reach the registry through the wire"
        );
    }

    #[test]
    fn with_display_name_overrides_default() {
        let spec = ProcessorSpec::new(
            class_import_path("my_app.filters:BlurProcessor"),
            serde_json::Value::Null,
        )
        .with_display_name("Camera A");
        assert_eq!(spec.display_name.as_deref(), Some("Camera A"));
    }

    /// msgpack `to_vec_named` → `from_slice` round-trip preserves full value
    /// equality, over both identity grammars and a unicode display name.
    #[test]
    fn msgpack_round_trip_preserves_full_value() {
        for path in [
            "my_app.filters:BlurProcessor",
            "my_app::filters::BlurProcessor",
        ] {
            let spec = ProcessorSpec::new(
                class_import_path(path),
                serde_json::json!({
                    "width": 1920,
                    "label": "カメラ — 中文 — emoji 🎥",
                    "nested": {"key": "value", "arr": [1, 2, 3]},
                }),
            )
            .with_display_name("こんにちは");

            let bytes = rmp_serde::to_vec_named(&spec).expect("encode");
            let back: ProcessorSpec = rmp_serde::from_slice(&bytes).expect("decode");
            assert_eq!(spec, back, "{path} lost equality over msgpack");
        }
    }

    /// Empty config + absent display_name still round-trips.
    #[test]
    fn msgpack_round_trip_minimal_spec() {
        let spec = ProcessorSpec::new(
            class_import_path("my_app.filters:BlurProcessor"),
            serde_json::Value::Null,
        );
        let bytes = rmp_serde::to_vec_named(&spec).expect("encode");
        let back: ProcessorSpec = rmp_serde::from_slice(&bytes).expect("decode");
        assert_eq!(spec, back);
        assert!(back.display_name.is_none());
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
            let spec = ProcessorSpec::new(class_import_path("my_app:T"), payload.clone());
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
