// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The JSON Schema a processor descriptor carries for its config type.
//!
//! One dialect leaves this seam: JSON Schema draft 2020-12 with no `$schema`
//! key. `schemars` 0.8 emits draft-07, and the conversion is three things:
//! the meta-schema key and the pointer prefix, which the generator's own
//! settings decide; the root keyword `definitions`, which `RootSchema`
//! hard-codes; and a tuple field's positional item schemas, which draft-07
//! spells as an array-valued `items`.
//!
//! A config key's description is the one its `#[description = …]` attribute
//! declares under `#[processor_config]`, and nothing else: the doc comment
//! `schemars` reads into a `description` or a `title` is the code's own
//! documentation and never reaches the document.

use std::borrow::Cow;
use std::marker::PhantomData;

use schemars::JsonSchema;
use schemars::r#gen::SchemaGenerator;
use schemars::schema::{RootSchema, Schema, SchemaObject, SingleOrVec};
use schemars::visit::{Visitor, visit_root_schema, visit_schema_object};
use serde_json::Value;

/// Where a `$ref` points in a 2020-12 document.
const DEFINITION_POINTER_PREFIX: &str = "#/$defs/";

/// Marks a type usable as a processor's `config =` type, and derives its
/// schema document.
///
/// Carries the `JsonSchema` bound so that a config type without the derive
/// fails on this trait, whose note names the fix, rather than on a bare
/// `schemars` bound the author has no path to.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is a processor `config =` type but does not derive `JsonSchema`",
    note = "add `#[derive(streamlib::sdk::schemars::JsonSchema)]` and `#[schemars(crate = \"streamlib::sdk::schemars\")]` to `{Self}`",
    note = "the SDK re-exports `schemars` at `streamlib::sdk::schemars`, so the crate needs no new dependency"
)]
pub trait ProcessorConfigJsonSchema {
    /// This config type's schema, as JSON Schema draft 2020-12.
    fn processor_config_schema_document() -> Value;
}

impl<T: schemars::JsonSchema> ProcessorConfigJsonSchema for T {
    fn processor_config_schema_document() -> Value {
        let root_schema = schemars::r#gen::SchemaSettings::draft07()
            .with(|settings| {
                settings.meta_schema = None;
                settings.definitions_path = DEFINITION_POINTER_PREFIX.to_string();
            })
            .with_visitor(TupleItemsRewrittenAsPrefixItems)
            .with_visitor(OnlyDeclaredDescriptionsKept)
            .into_generator()
            .into_root_schema_for::<T>();

        let mut document =
            serde_json::to_value(root_schema).expect("a schemars root schema always serializes");
        rename_the_root_definitions_keyword_to_defs(&mut document);
        document
    }
}

/// Marks a `#[processor_config]` type whose `Default` fills each key a config
/// leaves out, and which a node placed with no config runs with.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is a `#[processor_config]` type but does not implement `Default`",
    note = "derive `Default` on `{Self}`, or implement it, so every key a config leaves out is filled from it",
    note = "`Default` must be a config the node runs with: a key with nothing sensible to default to is left unset, and the node reports itself not ready, naming the key, until it is set"
)]
pub trait ProcessorConfigFilledFromItsDefault: Default {}

impl<T: Default> ProcessorConfigFilledFromItsDefault for T {}

/// Where a schema carries the description a `#[description = …]` attribute
/// declared until the document is rendered, apart from the `description` a
/// doc comment fills.
const DECLARED_DESCRIPTION_EXTENSION_KEY: &str = "x-streamlib-declared-description";

/// The text one config key's `#[description = …]` attribute declares.
pub trait ProcessorConfigKeyDescription {
    /// The description, CommonMark with no raw HTML, served exactly as written.
    const DESCRIPTION: &'static str;
}

/// The schema `#[processor_config]` gives a described key in place of its
/// type's: `T`'s, carrying the description `D` declares.
pub struct ProcessorConfigKeyDescribedBy<T: ?Sized, D>(PhantomData<D>, PhantomData<Box<T>>);

impl<T: JsonSchema + ?Sized, D: ProcessorConfigKeyDescription> JsonSchema
    for ProcessorConfigKeyDescribedBy<T, D>
{
    fn is_referenceable() -> bool {
        false
    }

    fn schema_name() -> String {
        T::schema_name()
    }

    fn schema_id() -> Cow<'static, str> {
        T::schema_id()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        schema_carrying_the_declared_description(generator.subschema_for::<T>(), D::DESCRIPTION)
    }

    fn _schemars_private_non_optional_json_schema(generator: &mut SchemaGenerator) -> Schema {
        schema_carrying_the_declared_description(
            T::_schemars_private_non_optional_json_schema(generator),
            D::DESCRIPTION,
        )
    }

    fn _schemars_private_is_option() -> bool {
        T::_schemars_private_is_option()
    }
}

/// `schema`, marked with the description its key declares.
pub fn schema_carrying_the_declared_description(schema: Schema, description: &str) -> Schema {
    let mut schema_object = schema.into_object();
    schema_object.extensions.insert(
        DECLARED_DESCRIPTION_EXTENSION_KEY.to_string(),
        Value::String(description.to_string()),
    );
    Schema::Object(schema_object)
}

/// Drops every `description` and `title` a doc comment filled, then gives each
/// schema the description its attribute declared. The root keeps the `title`
/// the generator names it by.
#[derive(Debug, Clone)]
struct OnlyDeclaredDescriptionsKept;

impl Visitor for OnlyDeclaredDescriptionsKept {
    fn visit_root_schema(&mut self, root: &mut RootSchema) {
        let root_title = root
            .schema
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.title.clone());
        visit_root_schema(self, root);
        if root_title.is_some() {
            root.schema.metadata().title = root_title;
        }
    }

    fn visit_schema_object(&mut self, schema: &mut SchemaObject) {
        visit_schema_object(self, schema);
        if let Some(metadata) = schema.metadata.as_deref_mut() {
            metadata.description = None;
            metadata.title = None;
        }
        if let Some(Value::String(declared_description)) =
            schema.extensions.remove(DECLARED_DESCRIPTION_EXTENSION_KEY)
        {
            schema.metadata().description = Some(declared_description);
        }
    }
}

/// `RootSchema` serializes its definitions map under the draft-07 keyword
/// whatever the generator's pointer prefix says, so the root key is renamed
/// here to match the `#/$defs/` the references already carry.
fn rename_the_root_definitions_keyword_to_defs(document: &mut Value) {
    if let Some(root_object) = document.as_object_mut()
        && let Some(definitions) = root_object.remove("definitions")
    {
        root_object.insert("$defs".to_string(), definitions);
    }
}

/// Rewrites a tuple's positional item schemas into the keyword 2020-12 reads
/// them under.
///
/// Draft-07 says positional schemas with an array-valued `items` and bounds
/// the rest with `additionalItems`; 2020-12 says `prefixItems` and lets
/// `items` mean the rest. A single-schema `items` means the same thing in both
/// and is left alone. Typed rather than a walk over the serialized document,
/// so a config type whose own `default` or `enum` data happens to hold a key
/// named `items` is never touched.
#[derive(Debug, Clone)]
struct TupleItemsRewrittenAsPrefixItems;

impl Visitor for TupleItemsRewrittenAsPrefixItems {
    fn visit_schema_object(&mut self, schema: &mut SchemaObject) {
        visit_schema_object(self, schema);

        let positional_item_schemas = match schema.array.as_deref_mut() {
            Some(array) if matches!(array.items, Some(SingleOrVec::Vec(_))) => {
                let Some(SingleOrVec::Vec(positional_item_schemas)) = array.items.take() else {
                    return;
                };
                array.items = array.additional_items.take().map(SingleOrVec::Single);
                positional_item_schemas
            }
            _ => return,
        };

        schema.extensions.insert(
            "prefixItems".to_string(),
            Value::Array(
                positional_item_schemas
                    .into_iter()
                    .map(|item_schema| {
                        serde_json::to_value(item_schema).expect("a schema always serializes")
                    })
                    .collect(),
            ),
        );
    }
}

/// The single-schema `items` the visitor left untouched, for the tests below.
#[cfg(test)]
fn single_schema_items(schema: &SchemaObject) -> Option<&schemars::schema::Schema> {
    match schema.array.as_deref()?.items.as_ref()? {
        SingleOrVec::Single(item_schema) => Some(item_schema),
        SingleOrVec::Vec(_) => None,
    }
}

#[cfg(test)]
mod config_schema_document_tests {
    use super::*;
    use schemars::JsonSchema;
    use schemars::schema::Schema;
    use serde::{Deserialize, Serialize};

    /// The shape a served document is checked against: a declared description
    /// is the key's description, a doc comment never is, serde defaults become
    /// defaults, and a field without one is required.
    #[derive(Serialize, Deserialize, JsonSchema)]
    struct FixtureSourceConfig {
        /// Read by rustdoc, never by the document.
        #[serde(default = "default_width")]
        #[schemars(with = "ProcessorConfigKeyDescribedBy<u32, FixtureWidthDescription>")]
        width: u32,
        /// Where the recording is written.
        path: String,
        /// How the frame maps onto the window.
        #[serde(default)]
        #[schemars(
            with = "ProcessorConfigKeyDescribedBy<FixtureScaling, FixtureScalingDescription>"
        )]
        scaling: FixtureScaling,
        /// Left, top, right, bottom.
        crop: (u32, u32, u32, u32),
        /// Every device to open.
        device_ids: Vec<String>,
        #[schemars(with = "ProcessorConfigKeyDescribedBy<Option<u32>, FixtureFpsDescription>")]
        fps: Option<u32>,
    }

    const FIXTURE_FPS_DESCRIPTION: &str = "Frames per second; absent, the device's own.";

    struct FixtureWidthDescription;
    impl ProcessorConfigKeyDescription for FixtureWidthDescription {
        const DESCRIPTION: &'static str = "Frame width in **pixels**.";
    }

    struct FixtureScalingDescription;
    impl ProcessorConfigKeyDescription for FixtureScalingDescription {
        const DESCRIPTION: &'static str = "How the frame maps onto the window.";
    }

    struct FixtureFpsDescription;
    impl ProcessorConfigKeyDescription for FixtureFpsDescription {
        const DESCRIPTION: &'static str = FIXTURE_FPS_DESCRIPTION;
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
        let document = FixtureSourceConfig::processor_config_schema_document();
        let width = &document["properties"]["width"];
        assert_eq!(width["type"], "integer");
        assert_eq!(width["description"], "Frame width in **pixels**.");
        assert_eq!(width["default"], 1280);
    }

    #[test]
    fn a_declared_description_is_served_on_a_key_whose_schema_is_a_reference() {
        let document = FixtureSourceConfig::processor_config_schema_document();
        let scaling = &document["properties"]["scaling"];
        assert_eq!(scaling["allOf"][0]["$ref"], "#/$defs/FixtureScaling");
        assert_eq!(
            scaling["description"],
            "How the frame maps onto the window."
        );
    }

    #[test]
    fn a_description_held_in_a_const_is_served_and_an_optional_key_stays_optional() {
        let document = FixtureSourceConfig::processor_config_schema_document();
        assert_eq!(
            document["properties"]["fps"]["description"],
            FIXTURE_FPS_DESCRIPTION
        );
        let required = document["required"].as_array().expect("a required list");
        assert!(!required.contains(&Value::String("fps".to_string())));
    }

    #[test]
    fn no_doc_comment_reaches_the_document_as_a_description_or_a_title() {
        let document = FixtureSourceConfig::processor_config_schema_document();
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
        let document = FixtureSourceConfig::processor_config_schema_document();
        let required = document["required"].as_array().expect("a required list");
        assert!(required.contains(&Value::String("path".to_string())));
        assert!(!required.contains(&Value::String("width".to_string())));
        assert!(!required.contains(&Value::String("scaling".to_string())));
    }

    #[test]
    fn the_document_is_2020_12_with_no_schema_key_and_no_definitions_keyword() {
        let document = FixtureSourceConfig::processor_config_schema_document();
        assert!(document.get("$schema").is_none());
        assert!(document.get("definitions").is_none());
        assert!(document["$defs"]["FixtureScaling"].is_object());
    }

    #[test]
    fn a_nested_types_reference_points_into_defs_rather_than_definitions() {
        let document = FixtureSourceConfig::processor_config_schema_document();
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
        let document = FixtureSourceConfig::processor_config_schema_document();
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
        let document = FixtureSourceConfig::processor_config_schema_document();
        let device_ids = &document["properties"]["device_ids"];
        assert_eq!(device_ids["type"], "array");
        assert_eq!(device_ids["items"]["type"], "string");
        assert!(device_ids.get("prefixItems").is_none(), "{device_ids}");
    }

    /// A tuple's draft-07 `additionalItems` is what 2020-12 calls `items`.
    #[test]
    fn a_bounded_tuple_tail_becomes_the_items_keyword() {
        let mut schema = SchemaObject {
            array: Some(Box::new(schemars::schema::ArrayValidation {
                items: Some(SingleOrVec::Vec(vec![Schema::Bool(true)])),
                additional_items: Some(Box::new(Schema::Bool(false))),
                ..Default::default()
            })),
            ..Default::default()
        };
        TupleItemsRewrittenAsPrefixItems.visit_schema_object(&mut schema);

        assert_eq!(
            single_schema_items(&schema),
            Some(&Schema::Bool(false)),
            "the draft-07 tail schema becomes 2020-12's `items`"
        );
        assert_eq!(
            schema.extensions.get("prefixItems"),
            Some(&serde_json::json!([true]))
        );
    }
}
