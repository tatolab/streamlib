// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A config value naming another node of the same graph: the reference edge's
//! one spelling in a config, `{"node": "<name>"}`, and the schema keyword that
//! marks a config key as holding one.

use std::borrow::Cow;

use schemars::JsonSchema;
use schemars::r#gen::SchemaGenerator;
use schemars::schema::{InstanceType, ObjectValidation, Schema, SchemaObject};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::descriptors::PortDescriptor;
use crate::pin_type::{DeclaredPinType, PinType};

/// The schema keyword a config key holding a node reference carries, of an
/// empty object. Such a key takes a node of any type; an input declaring a
/// `NodeReference` names the type and whether one node at most names a node.
pub const NODE_REFERENCE_SCHEMA_KEYWORD: &str = "x-tatolab-node-reference";

/// The key of the object a node reference is written as.
pub const NODE_REFERENCE_NODE_KEY: &str = "node";

/// A config value naming another node of the graph, of any type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
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
        node_reference_schema(generator)
    }
}

fn node_reference_schema(generator: &mut SchemaGenerator) -> Schema {
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
        Value::Object(serde_json::Map::new()),
    );
    Schema::Object(schema_object)
}

/// One config key holding a node reference: a top-level key a node type's config
/// schema marks, or an input declaring a `NodeReference` or a `Connection`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigKeyHoldingANodeReference {
    /// The config key.
    pub config_key: String,
    /// The import path of the type the referenced node must be, as the input's
    /// `NodeReference` names it; `None` takes a node of any type.
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
                .marks_a_node_reference(property_schema, &mut Vec::new())
                .then(|| ConfigKeyHoldingANodeReference {
                    config_key: config_key.clone(),
                    referenced_node_type_import_path: None,
                    one_referrer_per_referenced_node: false,
                    takes_a_value_other_than_a_node_reference: schema_walk
                        .takes_a_value_other_than_a_node_reference_or_null(
                            property_schema,
                            &mut Vec::new(),
                        ),
                })
        })
        .collect()
}

/// Every input among `inputs` that holds a node reference, in their order: each one declaring
/// exactly a `NodeReference`, read as a config key of its name, and each one declaring exactly a
/// `Connection`, which holds the node its wire brings once the wire is lowered, of any node type
/// since the wire's connection type judged it. Such an input takes nothing but a reference or no
/// value.
pub fn inputs_holding_a_node_reference<'input>(
    inputs: impl IntoIterator<Item = &'input PortDescriptor>,
) -> Vec<ConfigKeyHoldingANodeReference> {
    inputs
        .into_iter()
        .filter_map(|input| {
            let DeclaredPinType::Is { pin_type } = input.declared_pin()?.pin_type else {
                return None;
            };
            let (referenced_node_type_import_path, one_referrer_per_referenced_node) =
                match pin_type {
                    PinType::NodeReference {
                        referenced_node_type,
                        one_referrer_per_referenced_node,
                    } => (
                        referenced_node_type.map(|node_type| node_type.as_str().to_string()),
                        one_referrer_per_referenced_node,
                    ),
                    PinType::Connection {
                        one_referrer_per_referenced_node,
                        ..
                    } => (None, one_referrer_per_referenced_node),
                    _ => return None,
                };
            Some(ConfigKeyHoldingANodeReference {
                config_key: input.name.clone(),
                referenced_node_type_import_path,
                takes_a_value_other_than_a_node_reference: false,
                one_referrer_per_referenced_node,
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

    fn marks_a_node_reference(
        &self,
        schema: &'schema Value,
        refs_followed_on_this_path: &mut Vec<&'schema str>,
    ) -> bool {
        if schema
            .get(NODE_REFERENCE_SCHEMA_KEYWORD)
            .is_some_and(Value::is_object)
        {
            return true;
        }
        if let Some((pointer, definition)) =
            self.definition_a_ref_names(schema, refs_followed_on_this_path)
        {
            refs_followed_on_this_path.push(pointer);
            let found = self.marks_a_node_reference(definition, refs_followed_on_this_path);
            refs_followed_on_this_path.pop();
            if found {
                return true;
            }
        }
        Self::combinator_branches(schema)
            .any(|branch| self.marks_a_node_reference(branch, refs_followed_on_this_path))
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
    use crate::NodeConfigJsonSchema;

    #[derive(JsonSchema, Serialize, Deserialize)]
    #[allow(
        dead_code,
        reason = "the struct exists only for its derived JSON schema"
    )]
    struct ConfigNamingARequiredAndAnOptionalNode {
        gateway: NodeReferenceInConfig,
        anything: Option<NodeReferenceInConfig>,
        track: String,
    }

    #[test]
    fn a_derived_config_schema_marks_each_key_holding_a_node_reference() {
        let document = ConfigNamingARequiredAndAnOptionalNode::node_config_schema_document();
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
                    referenced_node_type_import_path: None,
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
                "allOf": [{NODE_REFERENCE_SCHEMA_KEYWORD: {}}]
            }},
            "$defs": {"Described": {"description": "the gateway"}}
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
    fn a_reference_round_trips_as_the_node_object() {
        let reference = NodeReferenceInConfig {
            node: "cloudflare".to_string(),
        };
        let written = serde_json::to_value(&reference).unwrap();
        assert_eq!(written, serde_json::json!({"node": "cloudflare"}));
        let read: NodeReferenceInConfig = serde_json::from_value(written).unwrap();
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
