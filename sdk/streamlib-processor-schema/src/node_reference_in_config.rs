// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A config value naming another node of the same graph: the reference edge's
//! one spelling in a config, `{"node": "<name>"}`, and the schema keyword that
//! marks a config key as holding one.

use std::borrow::Cow;
use std::marker::PhantomData;

use schemars::JsonSchema;
use schemars::r#gen::SchemaGenerator;
use schemars::schema::{InstanceType, ObjectValidation, Schema, SchemaObject};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// The schema keyword a config key holding a node reference carries. Its value
/// is an object; its `node_type` key, when present, is the import path the
/// referenced node must be, and its `one_referrer_per_referenced_node` key,
/// when `true`, refuses a second node whose key names the same node.
pub const NODE_REFERENCE_SCHEMA_KEYWORD: &str = "x-tatolab-node-reference";

/// The key of the object a node reference is written as.
pub const NODE_REFERENCE_NODE_KEY: &str = "node";

/// The keyword's key naming the node type a reference must name.
const NODE_REFERENCE_SCHEMA_NODE_TYPE_KEY: &str = "node_type";

/// The keyword's key that, when `true`, lets one node at most name a given
/// node under the config key.
pub const NODE_REFERENCE_SCHEMA_ONE_REFERRER_PER_REFERENCED_NODE_KEY: &str =
    "one_referrer_per_referenced_node";

/// A config value naming another node of the graph, of any type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeReferenceInConfig {
    /// The referenced node's name, as `add` gave it.
    pub node: String,
}

impl JsonSchema for NodeReferenceInConfig {
    fn is_referenceable() -> bool {
        false
    }

    fn schema_name() -> String {
        "NodeReferenceInConfig".to_string()
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed(concat!(module_path!(), "::NodeReferenceInConfig"))
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        node_reference_schema(generator, None)
    }
}

/// A node type a typed node reference may name, by its import path.
pub trait NodeTypeANodeReferenceNames {
    /// The import path of the node type, as a graph's `type` names it.
    fn node_type_import_path() -> &'static str;
}

/// A config value naming another node of the graph, which must be of type `T`.
pub struct NodeReferenceToNodeType<T: NodeTypeANodeReferenceNames> {
    /// The referenced node's name, as `add` gave it.
    pub node: String,
    referenced_node_type: PhantomData<fn() -> T>,
}

impl<T: NodeTypeANodeReferenceNames> NodeReferenceToNodeType<T> {
    /// A reference to the node named `node`.
    pub fn new(node: impl Into<String>) -> Self {
        Self {
            node: node.into(),
            referenced_node_type: PhantomData,
        }
    }
}

impl<T: NodeTypeANodeReferenceNames> std::fmt::Debug for NodeReferenceToNodeType<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NodeReferenceToNodeType")
            .field("node", &self.node)
            .field("node_type", &T::node_type_import_path())
            .finish()
    }
}

impl<T: NodeTypeANodeReferenceNames> Clone for NodeReferenceToNodeType<T> {
    fn clone(&self) -> Self {
        Self::new(self.node.clone())
    }
}

impl<T: NodeTypeANodeReferenceNames> PartialEq for NodeReferenceToNodeType<T> {
    fn eq(&self, other: &Self) -> bool {
        self.node == other.node
    }
}

impl<T: NodeTypeANodeReferenceNames> Serialize for NodeReferenceToNodeType<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        NodeReferenceInConfig {
            node: self.node.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de, T: NodeTypeANodeReferenceNames> Deserialize<'de> for NodeReferenceToNodeType<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        NodeReferenceInConfig::deserialize(deserializer).map(|untyped| Self::new(untyped.node))
    }
}

impl<T: NodeTypeANodeReferenceNames> JsonSchema for NodeReferenceToNodeType<T> {
    fn is_referenceable() -> bool {
        false
    }

    fn schema_name() -> String {
        format!("NodeReferenceTo_{}", T::node_type_import_path())
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Owned(format!(
            "{}::NodeReferenceToNodeType<{}>",
            module_path!(),
            T::node_type_import_path()
        ))
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        node_reference_schema(generator, Some(T::node_type_import_path()))
    }
}

fn node_reference_schema(
    generator: &mut SchemaGenerator,
    referenced_node_type: Option<&str>,
) -> Schema {
    let mut keyword = serde_json::Map::new();
    if let Some(node_type) = referenced_node_type {
        keyword.insert(
            NODE_REFERENCE_SCHEMA_NODE_TYPE_KEY.to_string(),
            Value::String(node_type.to_string()),
        );
    }
    let mut object_validation = ObjectValidation::default();
    object_validation.properties.insert(
        NODE_REFERENCE_NODE_KEY.to_string(),
        generator.subschema_for::<String>(),
    );
    object_validation
        .required
        .insert(NODE_REFERENCE_NODE_KEY.to_string());
    object_validation.additional_properties = Some(Box::new(Schema::Bool(false)));
    let mut schema_object = SchemaObject {
        instance_type: Some(InstanceType::Object.into()),
        object: Some(Box::new(object_validation)),
        ..SchemaObject::default()
    };
    schema_object.extensions.insert(
        NODE_REFERENCE_SCHEMA_KEYWORD.to_string(),
        Value::Object(keyword),
    );
    Schema::Object(schema_object)
}

/// One top-level config key a node type's config schema marks as holding a
/// node reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigKeyHoldingANodeReference {
    /// The config key.
    pub config_key: String,
    /// The import path of the type the referenced node must be, as the
    /// schema writes it; `None` takes a node of any type.
    pub referenced_node_type_import_path: Option<String>,
    /// Whether the key's schema also takes a value that is neither a node
    /// reference nor `null`, as `NodeReference | str` does.
    pub takes_a_value_other_than_a_node_reference: bool,
    /// Whether one node at most may name a given node under this key, as one
    /// send node at most writes a track.
    pub one_referrer_per_referenced_node: bool,
}

/// Every top-level key of `config_schema` that holds a node reference, in the
/// schema's property order. A key's schema marks one directly, through a
/// `$ref` into the document's `$defs`, or as one branch of an `anyOf`, `oneOf`
/// or `allOf`, which is how an optional reference is written.
pub fn config_keys_holding_a_node_reference(
    config_schema: &Value,
) -> Vec<ConfigKeyHoldingANodeReference> {
    let Some(properties) = config_schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    properties
        .iter()
        .filter_map(|(config_key, property_schema)| {
            let schema_walk = SchemaDocumentWalk {
                document: config_schema,
            };
            schema_walk
                .node_reference_keyword_in(property_schema, &mut Vec::new())
                .map(|keyword| ConfigKeyHoldingANodeReference {
                    config_key: config_key.clone(),
                    referenced_node_type_import_path: keyword
                        .get(NODE_REFERENCE_SCHEMA_NODE_TYPE_KEY)
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    one_referrer_per_referenced_node: keyword
                        .get(NODE_REFERENCE_SCHEMA_ONE_REFERRER_PER_REFERENCED_NODE_KEY)
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    takes_a_value_other_than_a_node_reference: schema_walk
                        .takes_a_value_other_than_a_node_reference_or_null(
                            property_schema,
                            &mut Vec::new(),
                        ),
                })
        })
        .collect()
}

/// A walk through one config schema document, following each `$ref` once per
/// path so a recursive definition ends it.
struct SchemaDocumentWalk<'schema> {
    document: &'schema Value,
}

impl<'schema> SchemaDocumentWalk<'schema> {
    fn definition_a_ref_names(
        &self,
        schema: &'schema Value,
        refs_followed_on_this_path: &[&'schema str],
    ) -> Option<(&'schema str, &'schema Value)> {
        let pointer = schema.get("$ref").and_then(Value::as_str)?;
        if refs_followed_on_this_path.contains(&pointer) {
            return None;
        }
        let definition = self.document.pointer(pointer.strip_prefix('#')?)?;
        Some((pointer, definition))
    }

    fn combinator_branches(schema: &'schema Value) -> impl Iterator<Item = &'schema Value> {
        ["anyOf", "oneOf", "allOf"]
            .into_iter()
            .filter_map(|combinator| schema.get(combinator).and_then(Value::as_array))
            .flatten()
    }

    fn node_reference_keyword_in(
        &self,
        schema: &'schema Value,
        refs_followed_on_this_path: &mut Vec<&'schema str>,
    ) -> Option<&'schema serde_json::Map<String, Value>> {
        if let Some(keyword) = schema
            .get(NODE_REFERENCE_SCHEMA_KEYWORD)
            .and_then(Value::as_object)
        {
            return Some(keyword);
        }
        if let Some((pointer, definition)) =
            self.definition_a_ref_names(schema, refs_followed_on_this_path)
        {
            refs_followed_on_this_path.push(pointer);
            let found = self.node_reference_keyword_in(definition, refs_followed_on_this_path);
            refs_followed_on_this_path.pop();
            if found.is_some() {
                return found;
            }
        }
        Self::combinator_branches(schema)
            .find_map(|branch| self.node_reference_keyword_in(branch, refs_followed_on_this_path))
    }

    /// Whether `schema` takes a value that is neither a node reference nor
    /// `null`: its own `$ref`, some `anyOf` or `oneOf` branch and every
    /// `allOf` branch each take one.
    fn takes_a_value_other_than_a_node_reference_or_null(
        &self,
        schema: &'schema Value,
        refs_followed_on_this_path: &mut Vec<&'schema str>,
    ) -> bool {
        if schema.get(NODE_REFERENCE_SCHEMA_KEYWORD).is_some()
            || schema.get("type").and_then(Value::as_str) == Some("null")
        {
            return false;
        }
        let mut takes_another_value = true;
        if let Some((pointer, definition)) =
            self.definition_a_ref_names(schema, refs_followed_on_this_path)
        {
            refs_followed_on_this_path.push(pointer);
            takes_another_value &= self.takes_a_value_other_than_a_node_reference_or_null(
                definition,
                refs_followed_on_this_path,
            );
            refs_followed_on_this_path.pop();
        }
        let alternatives: Vec<&Value> = ["anyOf", "oneOf"]
            .into_iter()
            .filter_map(|combinator| schema.get(combinator).and_then(Value::as_array))
            .flatten()
            .collect();
        if !alternatives.is_empty() {
            takes_another_value &= alternatives.into_iter().any(|branch| {
                self.takes_a_value_other_than_a_node_reference_or_null(
                    branch,
                    refs_followed_on_this_path,
                )
            });
        }
        if let Some(all_of) = schema.get("allOf").and_then(Value::as_array) {
            takes_another_value &= all_of.iter().all(|branch| {
                self.takes_a_value_other_than_a_node_reference_or_null(
                    branch,
                    refs_followed_on_this_path,
                )
            });
        }
        takes_another_value
    }
}

/// The name a config value written as a node reference names, `None` when the
/// value is not `{"node": "<name>"}`.
pub fn node_name_a_config_value_references(config_value: &Value) -> Option<&str> {
    let object = config_value.as_object()?;
    if object.len() != 1 {
        return None;
    }
    object.get(NODE_REFERENCE_NODE_KEY)?.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProcessorConfigJsonSchema;

    #[test]
    fn a_key_whose_keyword_takes_one_referrer_per_referenced_node_says_so() {
        let schema = serde_json::json!({
            "properties": {
                "track": {NODE_REFERENCE_SCHEMA_KEYWORD: {
                    "node_type": "tatolab.moq:MoqTrack",
                    NODE_REFERENCE_SCHEMA_ONE_REFERRER_PER_REFERENCED_NODE_KEY: true,
                }},
            }
        });
        assert_eq!(
            config_keys_holding_a_node_reference(&schema),
            vec![ConfigKeyHoldingANodeReference {
                config_key: "track".to_string(),
                referenced_node_type_import_path: Some("tatolab.moq:MoqTrack".to_string()),
                takes_a_value_other_than_a_node_reference: false,
                one_referrer_per_referenced_node: true,
            }]
        );
    }

    struct GatewayNodeTypeForTheTest;

    impl NodeTypeANodeReferenceNames for GatewayNodeTypeForTheTest {
        fn node_type_import_path() -> &'static str {
            "tatolab.moq:MoqGateway"
        }
    }

    #[derive(JsonSchema, Serialize, Deserialize)]
    #[allow(dead_code)]
    struct ConfigNamingAGatewayAndAnyNode {
        gateway: NodeReferenceToNodeType<GatewayNodeTypeForTheTest>,
        anything: Option<NodeReferenceInConfig>,
        track: String,
    }

    #[test]
    fn a_derived_config_schema_marks_each_key_holding_a_node_reference() {
        let document = ConfigNamingAGatewayAndAnyNode::processor_config_schema_document();
        assert_eq!(
            config_keys_holding_a_node_reference(&document),
            vec![
                ConfigKeyHoldingANodeReference {
                    config_key: "anything".to_string(),
                    referenced_node_type_import_path: None,
                    takes_a_value_other_than_a_node_reference: false,
                    one_referrer_per_referenced_node: false,
                },
                ConfigKeyHoldingANodeReference {
                    config_key: "gateway".to_string(),
                    referenced_node_type_import_path: Some("tatolab.moq:MoqGateway".to_string()),
                    takes_a_value_other_than_a_node_reference: false,
                    one_referrer_per_referenced_node: false,
                },
            ]
        );
    }

    #[test]
    fn a_reference_behind_a_ref_into_defs_is_found() {
        let document = serde_json::json!({
            "properties": {"gateway": {"$ref": "#/$defs/Gateway"}},
            "$defs": {"Gateway": {NODE_REFERENCE_SCHEMA_KEYWORD: {}}}
        });
        assert_eq!(
            config_keys_holding_a_node_reference(&document),
            vec![ConfigKeyHoldingANodeReference {
                config_key: "gateway".to_string(),
                referenced_node_type_import_path: None,
                takes_a_value_other_than_a_node_reference: false,
                one_referrer_per_referenced_node: false,
            }]
        );
    }

    #[test]
    fn a_reference_in_an_all_of_beside_an_unmarked_ref_is_found() {
        let document = serde_json::json!({
            "properties": {"gateway": {
                "$ref": "#/$defs/Described",
                "allOf": [{NODE_REFERENCE_SCHEMA_KEYWORD: {"node_type": "tatolab.moq:MoqGateway"}}]
            }},
            "$defs": {"Described": {"description": "the gateway"}}
        });
        assert_eq!(
            config_keys_holding_a_node_reference(&document),
            vec![ConfigKeyHoldingANodeReference {
                config_key: "gateway".to_string(),
                referenced_node_type_import_path: Some("tatolab.moq:MoqGateway".to_string()),
                takes_a_value_other_than_a_node_reference: false,
                one_referrer_per_referenced_node: false,
            }]
        );
    }

    #[test]
    fn a_reference_behind_a_long_chain_of_refs_is_found_and_a_cycle_ends_the_search() {
        let mut definitions = serde_json::Map::new();
        for link in 0..12 {
            definitions.insert(
                format!("Step{link}"),
                serde_json::json!({"$ref": format!("#/$defs/Step{}", link + 1)}),
            );
        }
        definitions.insert(
            "Step12".to_string(),
            serde_json::json!({NODE_REFERENCE_SCHEMA_KEYWORD: {}}),
        );
        definitions.insert(
            "Cycle".to_string(),
            serde_json::json!({"anyOf": [{"$ref": "#/$defs/Cycle"}, {"type": "null"}]}),
        );
        let document = serde_json::json!({
            "properties": {
                "deep": {"$ref": "#/$defs/Step0"},
                "cyclic": {"$ref": "#/$defs/Cycle"}
            },
            "$defs": definitions
        });
        let keys = config_keys_holding_a_node_reference(&document);
        assert_eq!(
            keys.iter()
                .map(|key| key.config_key.as_str())
                .collect::<Vec<_>>(),
            ["deep"]
        );
    }

    #[test]
    fn a_key_taking_a_reference_or_a_string_takes_another_value_and_an_optional_one_does_not() {
        let document = serde_json::json!({
            "properties": {
                "reference_or_name": {"anyOf": [{NODE_REFERENCE_SCHEMA_KEYWORD: {}}, {"type": "string"}]},
                "optional_reference": {"anyOf": [{NODE_REFERENCE_SCHEMA_KEYWORD: {}}, {"type": "null"}]}
            }
        });
        let takes_another_value: Vec<(String, bool)> =
            config_keys_holding_a_node_reference(&document)
                .into_iter()
                .map(|key| {
                    (
                        key.config_key,
                        key.takes_a_value_other_than_a_node_reference,
                    )
                })
                .collect();
        assert_eq!(
            takes_another_value,
            [
                ("reference_or_name".to_string(), true),
                ("optional_reference".to_string(), false)
            ]
        );
    }

    #[test]
    fn a_typed_reference_round_trips_as_the_node_object() {
        let reference = NodeReferenceToNodeType::<GatewayNodeTypeForTheTest>::new("cloudflare");
        let written = serde_json::to_value(&reference).unwrap();
        assert_eq!(written, serde_json::json!({"node": "cloudflare"}));
        let read: NodeReferenceToNodeType<GatewayNodeTypeForTheTest> =
            serde_json::from_value(written).unwrap();
        assert_eq!(read, reference);
    }

    #[test]
    fn only_the_node_object_reads_as_a_reference() {
        assert_eq!(
            node_name_a_config_value_references(&serde_json::json!({"node": "a"})),
            Some("a")
        );
        for not_a_reference in [
            serde_json::json!("a"),
            serde_json::json!({"node": 1}),
            serde_json::json!({"node": "a", "port": "b"}),
        ] {
            assert_eq!(node_name_a_config_value_references(&not_a_reference), None);
        }
    }
}
