// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use schemars::generate::SchemaSettings;
use schemars::transform::{Transform, transform_subschemas};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde_json::Value;

use crate::node_reference_in_config::{NODE_REFERENCE_SCHEMA_KEYWORD, NodeReferenceInConfig};

/// The keyword a 2020-12 document holds its definitions under.
const DEFINITIONS_KEYWORD: &str = "$defs";

/// Where a `$ref` points in a 2020-12 document.
const DEFINITION_POINTER_PREFIX: &str = "#/$defs/";

#[diagnostic::on_unimplemented(
    message = "`{Self}` is a node config type but does not derive `JsonSchema`",
    note = "add `#[derive(streamlib::sdk::schemars::JsonSchema)]` and `#[schemars(crate = \"streamlib::sdk::schemars\")]` to `{Self}`",
    note = "the SDK re-exports `schemars` at `streamlib::sdk::schemars`, so the crate needs no new dependency"
)]
pub trait NodeConfigJsonSchema {
    /// This config type's schema, as JSON Schema draft 2020-12.
    fn node_config_schema_document() -> Value;
}

impl<T: JsonSchema> NodeConfigJsonSchema for T {
    fn node_config_schema_document() -> Value {
        let mut generator = SchemaSettings::draft2020_12()
            .with(|settings| settings.meta_schema = None)
            .into_generator();
        let mut document = generator.root_schema_for::<T>();

        let root_title = document.remove("title");
        OnlyDeclaredDescriptionsKept.transform(&mut document);
        if let Some(root_title) = root_title {
            document.insert("title".to_string(), root_title);
        }
        mark_the_node_reference_definition(&mut generator, &mut document);
        document.to_value()
    }
}

/// Where a schema carries the description a `#[description = …]` attribute
/// declared until the document is rendered, apart from the `description` a
/// doc comment fills.
const DECLARED_DESCRIPTION_EXTENSION_KEY: &str = "x-streamlib-declared-description";

/// `schema`, marked with the description its key declares.
pub fn schema_carrying_the_declared_description(mut schema: Schema, description: &str) -> Schema {
    schema.insert(
        DECLARED_DESCRIPTION_EXTENSION_KEY.to_string(),
        Value::String(description.to_string()),
    );
    schema
}

/// Drops every `description` and `title` a doc comment filled, then gives each
/// schema the description its attribute declared.
#[derive(Debug, Clone)]
struct OnlyDeclaredDescriptionsKept;

impl Transform for OnlyDeclaredDescriptionsKept {
    fn transform(&mut self, schema: &mut Schema) {
        transform_subschemas(self, schema);
        let Some(schema_object) = schema.as_object_mut() else {
            return;
        };
        schema_object.remove("description");
        schema_object.remove("title");
        if let Some(declared_description @ Value::String(_)) =
            schema_object.remove(DECLARED_DESCRIPTION_EXTENSION_KEY)
        {
            schema_object.insert("description".to_string(), declared_description);
        }
    }
}

/// Marks the definition a config's node references point at with the keyword
/// that says a key holds one. The contract states the same type without it, so
/// the keyword is added to config documents only. The generator already named
/// the type's definition while building `document`, so asking it again returns
/// that name rather than registering a new one.
fn mark_the_node_reference_definition(generator: &mut SchemaGenerator, document: &mut Schema) {
    let reference = generator.subschema_for::<NodeReferenceInConfig>();
    let Some(definition_name) = reference
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|pointer| pointer.strip_prefix(DEFINITION_POINTER_PREFIX))
    else {
        return;
    };
    if let Some(Value::Object(definition)) = document
        .get_mut(DEFINITIONS_KEYWORD)
        .and_then(|definitions| definitions.get_mut(definition_name))
    {
        definition.insert(
            NODE_REFERENCE_SCHEMA_KEYWORD.to_string(),
            Value::Object(serde_json::Map::new()),
        );
    }
}

#[cfg(test)]
mod config_schema_document_tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    /// The shape a served document is checked against: a declared description
    /// is the key's description, a doc comment never is, serde defaults become
    /// defaults, and a field without one is required.
    #[derive(Serialize, Deserialize, JsonSchema)]
    struct FixtureSourceConfig {
        /// Read by rustdoc, never by the document.
        #[serde(default = "default_width")]
        #[schemars(schema_with = "fixture_width_schema")]
        width: u32,
        /// Where the recording is written.
        path: String,
        /// How the frame maps onto the window.
        #[serde(default)]
        #[schemars(schema_with = "fixture_scaling_schema")]
        scaling: FixtureScaling,
        /// Left, top, right, bottom.
        crop: (u32, u32, u32, u32),
        /// Every device to open.
        device_ids: Vec<String>,
        #[serde(default)]
        #[schemars(schema_with = "fixture_fps_schema")]
        fps: Option<u32>,
    }

    const FIXTURE_FPS_DESCRIPTION: &str = "Frames per second; absent, the device's own.";

    fn fixture_width_schema(generator: &mut SchemaGenerator) -> Schema {
        schema_carrying_the_declared_description(
            generator.subschema_for::<u32>(),
            "Frame width in **pixels**.",
        )
    }

    fn fixture_scaling_schema(generator: &mut SchemaGenerator) -> Schema {
        schema_carrying_the_declared_description(
            generator.subschema_for::<FixtureScaling>(),
            "How the frame maps onto the window.",
        )
    }

    fn fixture_fps_schema(generator: &mut SchemaGenerator) -> Schema {
        schema_carrying_the_declared_description(
            generator.subschema_for::<Option<u32>>(),
            FIXTURE_FPS_DESCRIPTION,
        )
    }

    fn default_width() -> u32 {
        1280
    }

    /// # Scaling
    ///
    /// A doc comment led by a heading is what `schemars` reads as a title.
    #[derive(Serialize, Deserialize, JsonSchema, Default)]
    #[serde(rename_all = "snake_case")]
    enum FixtureScaling {
        /// Letterboxed.
        #[default]
        Fit,
        Stretch,
    }

    #[test]
    fn a_config_types_document_carries_each_fields_type_description_and_default() {
        let document = FixtureSourceConfig::node_config_schema_document();
        let width = &document["properties"]["width"];
        assert_eq!(width["type"], "integer");
        assert_eq!(width["description"], "Frame width in **pixels**.");
        assert_eq!(width["default"], 1280);
    }

    #[test]
    fn a_declared_description_is_served_on_a_key_whose_schema_is_a_reference() {
        let document = FixtureSourceConfig::node_config_schema_document();
        let scaling = &document["properties"]["scaling"];
        assert_eq!(scaling["$ref"], "#/$defs/FixtureScaling");
        assert_eq!(
            scaling["description"],
            "How the frame maps onto the window."
        );
    }

    #[test]
    fn a_description_held_in_a_const_is_served_and_an_optional_key_stays_optional() {
        let document = FixtureSourceConfig::node_config_schema_document();
        assert_eq!(
            document["properties"]["fps"]["description"],
            FIXTURE_FPS_DESCRIPTION
        );
        let required = document["required"].as_array().expect("a required list");
        assert!(!required.contains(&Value::String("fps".to_string())));
    }

    #[test]
    fn no_doc_comment_reaches_the_document_as_a_description_or_a_title() {
        let document = FixtureSourceConfig::node_config_schema_document();
        let rendered = serde_json::to_string(&document).expect("the document serializes");
        for doc_comment_text in [
            "Read by rustdoc",
            "Where the recording is written",
            "Left, top, right, bottom",
            "Every device to open",
            "Letterboxed",
            "A doc comment led by a heading",
            "x-streamlib-declared-description",
        ] {
            assert!(
                !rendered.contains(doc_comment_text),
                "`{doc_comment_text}` must not reach the document: {rendered}"
            );
        }
        assert_eq!(document["title"], "FixtureSourceConfig");
    }

    #[test]
    fn a_field_serde_declares_no_default_for_is_required_and_one_it_does_is_not() {
        let document = FixtureSourceConfig::node_config_schema_document();
        let required = document["required"].as_array().expect("a required list");
        assert!(required.contains(&Value::String("path".to_string())));
        assert!(!required.contains(&Value::String("width".to_string())));
        assert!(!required.contains(&Value::String("scaling".to_string())));
    }

    #[test]
    fn the_document_is_2020_12_with_no_schema_key_and_no_definitions_keyword() {
        let document = FixtureSourceConfig::node_config_schema_document();
        assert!(document.get("$schema").is_none());
        assert!(document.get("definitions").is_none());
        assert!(document["$defs"]["FixtureScaling"].is_object());
    }

    #[test]
    fn a_nested_types_reference_points_into_defs_rather_than_definitions() {
        let document = FixtureSourceConfig::node_config_schema_document();
        let rendered = serde_json::to_string(&document).expect("the document serializes");
        assert!(
            !rendered.contains("#/definitions/"),
            "no draft-07 reference may survive: {rendered}"
        );
        assert!(
            rendered.contains("#/$defs/FixtureScaling"),
            "the nested enum's reference must point into $defs: {rendered}"
        );
    }

    /// Draft-07 spells a tuple's positional schemas as an array-valued
    /// `items`, which 2020-12 reads as a schema for every element and refuses
    /// as an array. A document labelled 2020-12 that carries the draft-07
    /// spelling is one a validator reads wrong with nothing to say so.
    #[test]
    fn a_tuple_fields_positional_schemas_are_named_prefix_items() {
        let document = FixtureSourceConfig::node_config_schema_document();
        let crop = &document["properties"]["crop"];
        assert_eq!(crop["type"], "array");
        assert_eq!(
            crop["prefixItems"].as_array().map(Vec::len),
            Some(4),
            "a four-tuple carries four positional schemas: {crop}"
        );
        assert!(
            crop.get("items").is_none(),
            "an unbounded tail was never declared, so nothing bounds it: {crop}"
        );
        assert!(crop.get("additionalItems").is_none(), "{crop}");
    }

    /// The single-schema spelling means the same thing in both drafts, so a
    /// plain sequence field must come through unchanged.
    #[test]
    fn a_sequence_fields_element_schema_is_left_as_items() {
        let document = FixtureSourceConfig::node_config_schema_document();
        let device_ids = &document["properties"]["device_ids"];
        assert_eq!(device_ids["type"], "array");
        assert_eq!(device_ids["items"]["type"], "string");
        assert!(device_ids.get("prefixItems").is_none(), "{device_ids}");
    }
}
