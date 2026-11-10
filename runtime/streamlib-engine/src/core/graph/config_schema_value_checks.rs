// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A node's config held to the config schema its node type publishes in the catalog.

use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::error::{Error, Result};
use crate::core::processors::NodeTypesOneStreamResolves;

/// Refuse `config` for node `node_name` of type `node_type` when the config schema the
/// type's descriptor carries refuses it, naming each field and what the schema states;
/// a type with no descriptor or no schema takes any config, and a `null` config is held
/// to the schema as the empty object it stands for.
pub(crate) fn refuse_a_config_its_node_types_config_schema_refuses(
    node_name: &str,
    node_type: &ProcessorClassImportPath,
    node_types: &NodeTypesOneStreamResolves,
    config: &serde_json::Value,
) -> Result<()> {
    let Some(config_schema) = node_types
        .descriptor(node_type)
        .and_then(|descriptor| descriptor.config_schema)
    else {
        return Ok(());
    };
    refuse_a_config_the_config_schema_refuses(node_name, node_type, &config_schema, config)
}

fn refuse_a_config_the_config_schema_refuses(
    node_name: &str,
    node_type: &ProcessorClassImportPath,
    config_schema: &serde_json::Value,
    config: &serde_json::Value,
) -> Result<()> {
    let config_schema_validator = jsonschema::validator_for(config_schema).map_err(|refusal| {
        Error::Configuration(format!(
            "node `{node_name}`'s type `{node_type}` publishes a config schema the runtime \
             cannot read: {refusal}"
        ))
    })?;
    let empty_config = serde_json::Value::Object(serde_json::Map::new());
    let config = if config.is_null() {
        &empty_config
    } else {
        config
    };
    let what_the_schema_refuses: Vec<String> = config_schema_validator
        .iter_errors(config)
        .map(|refusal| {
            let field = refusal.instance_path().to_string();
            if field.is_empty() {
                refusal.to_string()
            } else {
                format!("`{}`: {refusal}", field.trim_start_matches('/'))
            }
        })
        .collect();
    if what_the_schema_refuses.is_empty() {
        return Ok(());
    }
    Err(Error::Configuration(format!(
        "node `{node_name}`'s config is refused by the config schema of its type \
         `{node_type}`: {}",
        what_the_schema_refuses.join("; ")
    )))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const TEST_PATTERN: &str = "tatolab.testpattern:TestPatternSource";

    fn test_pattern_config_schema() -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "width": {"type": "integer", "minimum": 1, "default": 1280},
                "pattern": {"type": "string", "enum": ["bars", "gradient"]},
            },
            "required": ["pattern"],
            "additionalProperties": false,
        })
    }

    fn refusal_of(config: serde_json::Value) -> Result<()> {
        refuse_a_config_the_config_schema_refuses(
            "test-pattern",
            &ProcessorClassImportPath::new(TEST_PATTERN).unwrap(),
            &test_pattern_config_schema(),
            &config,
        )
    }

    #[test]
    fn a_config_the_schema_takes_is_taken() {
        refusal_of(json!({"width": 640, "pattern": "bars"})).unwrap();
    }

    #[test]
    fn a_string_where_the_schema_states_a_number_is_refused_naming_the_field_and_the_type() {
        let refusal = refusal_of(json!({"width": "wide", "pattern": "bars"}))
            .unwrap_err()
            .to_string();
        assert!(refusal.contains("node `test-pattern`"), "{refusal}");
        assert!(refusal.contains(TEST_PATTERN), "{refusal}");
        assert!(refusal.contains("`width`"), "{refusal}");
        assert!(refusal.contains("\"integer\""), "{refusal}");
    }

    #[test]
    fn every_field_the_schema_refuses_is_named() {
        let refusal = refusal_of(json!({"width": 0, "pattern": "plaid"}))
            .unwrap_err()
            .to_string();
        assert!(refusal.contains("`width`"), "{refusal}");
        assert!(refusal.contains("`pattern`"), "{refusal}");
    }

    #[test]
    fn a_missing_required_key_and_an_unknown_key_are_refused() {
        let refusal = refusal_of(json!({"colour": "red"}))
            .unwrap_err()
            .to_string();
        assert!(refusal.contains("\"pattern\""), "{refusal}");
        assert!(refusal.contains("colour"), "{refusal}");
    }

    #[test]
    fn a_null_config_is_held_to_the_schema_as_the_empty_object() {
        let refusal = refusal_of(serde_json::Value::Null).unwrap_err().to_string();
        assert!(
            refusal.contains("\"pattern\" is a required property"),
            "{refusal}"
        );
        refuse_a_config_the_config_schema_refuses(
            "test-pattern",
            &ProcessorClassImportPath::new(TEST_PATTERN).unwrap(),
            &json!({"type": "object", "properties": {}}),
            &serde_json::Value::Null,
        )
        .unwrap();
    }

    #[test]
    fn a_schema_that_states_nothing_about_a_field_takes_any_value() {
        refuse_a_config_the_config_schema_refuses(
            "test-pattern",
            &ProcessorClassImportPath::new(TEST_PATTERN).unwrap(),
            &json!({"type": "object", "properties": {"width": {}}}),
            &json!({"width": "wide", "anything": [1, 2]}),
        )
        .unwrap();
    }
}
