// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

use crate::core::descriptors::NodeClassImportPath;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeSpec {
    /// The import path of the class to instantiate — looked up in the registry
    /// verbatim.
    pub name: NodeClassImportPath,
    /// Every value set on the node, by input name: its settings and the values on its unlinked
    /// inputs alike.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub input_values: serde_json::Map<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The node's UUID from its graph document; `None` generates one as the node is placed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_uuid: Option<uuid::Uuid>,
    /// What the studio draws the node with: canvas position, group, comment, colour.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub studio: serde_json::Map<String, serde_json::Value>,
}

impl NodeSpec {
    /// Build a spec naming the class to instantiate by its import path, holding each value
    /// `input_values`, an object of values by input name, sets; `null` sets none. Any other value
    /// is a caller's mistake, which a debug build panics on and a release build reads as none.
    pub fn new(name: NodeClassImportPath, input_values: serde_json::Value) -> Self {
        debug_assert!(
            input_values.is_object() || input_values.is_null(),
            "a node spec's input values are an object by input name, not {input_values}"
        );
        let input_values = match input_values {
            serde_json::Value::Object(input_values) => input_values,
            _ => serde_json::Map::new(),
        };
        Self {
            input_values,
            ..Self::of_the_type(name)
        }
    }

    /// Build a spec naming the class to instantiate by its import path, holding no value.
    pub fn of_the_type(name: NodeClassImportPath) -> Self {
        Self {
            name,
            input_values: serde_json::Map::new(),
            display_name: None,
            node_uuid: None,
            studio: serde_json::Map::new(),
        }
    }

    /// The spec that places `node`, a node of a graph document of type `node_type`, as a load
    /// places it: its input values, its studio fields, its UUID, and its name unless it names
    /// none.
    pub(crate) fn of_a_graph_document_node(
        node_type: NodeClassImportPath,
        node: &streamlib_project_repository::StreamGraphDocumentNode,
    ) -> Self {
        let spec = Self::of_the_type(node_type)
            .with_input_values_and_studio_fields(node.input_values.clone(), node.studio.to_map())
            .with_node_uuid(node.id);
        if node.name.is_empty() {
            spec
        } else {
            spec.with_display_name(node.name.clone())
        }
    }

    /// Place the node under the UUID its graph document gives it.
    pub fn with_node_uuid(mut self, node_uuid: uuid::Uuid) -> Self {
        self.node_uuid = Some(node_uuid);
        self
    }

    /// Set each of `input_values` on the node too, over a value of its name it holds, and carry
    /// the graph document's studio fields onto it.
    pub fn with_input_values_and_studio_fields(
        mut self,
        input_values: serde_json::Map<String, serde_json::Value>,
        studio: serde_json::Map<String, serde_json::Value>,
    ) -> Self {
        self.input_values.extend(input_values);
        self.studio = studio;
        self
    }

    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        self.display_name = Some(display_name.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class_import_path(path: &str) -> NodeClassImportPath {
        NodeClassImportPath::new(path).expect("the fixture path names a class")
    }

    /// Wire-format lock — the class a spec names is a plain string on the
    /// wire, carrying no org, package or version key, because there is no
    /// longer anything for a reader to key on but the path itself.
    #[test]
    fn serde_emits_the_class_import_path_as_a_plain_string() {
        let spec = NodeSpec::new(
            class_import_path("my_app.filters:BlurNode"),
            serde_json::Value::Null,
        );
        let json: serde_json::Value = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            json["name"],
            serde_json::Value::String("my_app.filters:BlurNode".to_string())
        );
    }

    /// Pre-1.0 forbids parser shims: the three-key `{org, package, type}`
    /// object the wire used to carry must fail to deserialize rather than be
    /// accepted alongside the string.
    #[test]
    fn deserialize_refuses_the_structured_object_the_wire_used_to_carry() {
        let json = r#"{"name":{"org":"tatolab","package":"core","type":"Camera"}}"#;
        let refused: Result<NodeSpec, _> = serde_json::from_str(json);
        assert!(
            refused.is_err(),
            "the old three-key object must not deserialize — there is no back-compat shape"
        );
    }

    #[test]
    fn deserialize_refuses_a_name_that_names_no_class() {
        let refused: Result<NodeSpec, _> = serde_json::from_str(r#"{"name":""}"#);
        assert!(
            refused.is_err(),
            "an empty class path must not reach the registry through the wire"
        );
    }

    #[test]
    fn with_display_name_overrides_default() {
        let spec = NodeSpec::new(
            class_import_path("my_app.filters:BlurNode"),
            serde_json::Value::Null,
        )
        .with_display_name("Camera A");
        assert_eq!(spec.display_name.as_deref(), Some("Camera A"));
    }

    /// msgpack `to_vec_named` → `from_slice` round-trip preserves full value
    /// equality, over both identity grammars and a unicode display name.
    #[test]
    fn msgpack_round_trip_preserves_full_value() {
        for path in ["my_app.filters:BlurNode", "my_app::filters::BlurNode"] {
            let spec = NodeSpec::new(
                class_import_path(path),
                serde_json::json!({
                    "width": 1920,
                    "label": "カメラ — 中文 — emoji 🎥",
                    "nested": {"key": "value", "arr": [1, 2, 3]},
                }),
            )
            .with_display_name("こんにちは");

            let bytes = rmp_serde::to_vec_named(&spec).expect("encode");
            let back: NodeSpec = rmp_serde::from_slice(&bytes).expect("decode");
            assert_eq!(spec, back, "{path} lost equality over msgpack");
        }
    }

    /// No input value and no display_name still round-trips.
    #[test]
    fn msgpack_round_trip_minimal_spec() {
        let spec = NodeSpec::new(
            class_import_path("my_app.filters:BlurNode"),
            serde_json::Value::Null,
        );
        let bytes = rmp_serde::to_vec_named(&spec).expect("encode");
        let back: NodeSpec = rmp_serde::from_slice(&bytes).expect("decode");
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
    fn input_value_msgpack_round_trip_integer_axis() {
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
            let spec = NodeSpec::new(
                class_import_path("my_app:T"),
                serde_json::json!({ "value": payload.clone() }),
            );
            let bytes = rmp_serde::to_vec_named(&spec).expect("encode");
            let back: NodeSpec = rmp_serde::from_slice(&bytes).expect("decode");
            assert_eq!(
                spec, back,
                "{} round-trip lost equality at the Value level",
                name
            );
        }
    }
}
