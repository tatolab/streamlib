// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A relay adapter's settings as the fields a form draws, read off the
//! adapter's one Rust settings type.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::RelaySettingsJsonSchema;

/// The schema keyword a setting type writes to name its field type, read here
/// alone and never served.
pub(crate) const RELAY_SETTING_FIELD_TYPE_SCHEMA_KEYWORD: &str = "x-relay-setting-field-type";

/// The schema keyword a secret setting type writes, read here alone and never
/// served.
pub(crate) const RELAY_SETTING_SECRET_SCHEMA_KEYWORD: &str = "x-relay-setting-secret";

/// How a person enters a setting: the control a form draws it with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RelaySettingFieldType {
    /// One line of text.
    String,
    /// A URL.
    Url,
    /// A number.
    Number,
    /// On or off.
    Boolean,
    /// One of `options`.
    Select {
        /// The values it takes.
        options: Vec<String>,
    },
    /// A list of text values.
    StringList,
    /// A file's text, picked as a file or pasted.
    File,
}

/// One setting of a relay adapter, as the relay-adapter catalog lists it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct RelayAdapterSettingField {
    /// The key `add_relay` and `configure_relay` take it under in `settings`.
    pub name: String,
    /// How a person enters it.
    #[serde(flatten)]
    pub field_type: RelaySettingFieldType,
    /// Whether it is kept in the OS secret store and listed only as set or
    /// unset.
    pub secret: bool,
    /// Whether an instance's settings must carry it.
    pub required: bool,
    /// What it is, from the settings type's doc comment.
    pub doc: String,
    /// Its value when an instance's settings leave it out, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
}

/// The fields of the settings type `S`, in name order.
///
/// Panics when a field of `S` is of a type no field type covers, which each
/// adapter's catalog test reads.
pub fn relay_adapter_setting_fields_of<S: RelaySettingsJsonSchema>() -> Vec<RelayAdapterSettingField>
{
    relay_adapter_setting_fields_in_the_schema(&S::processor_config_schema_document())
        .unwrap_or_else(|undeclarable| panic!("{}: {undeclarable}", std::any::type_name::<S>()))
}

/// A settings field whose Rust type maps to no [`RelaySettingFieldType`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the setting `{setting_name}` has no field type: its schema is {property_schema}")]
pub(crate) struct RelaySettingFieldTypeUndeclarable {
    setting_name: String,
    property_schema: String,
}

fn relay_adapter_setting_fields_in_the_schema(
    settings_type_schema: &Value,
) -> Result<Vec<RelayAdapterSettingField>, RelaySettingFieldTypeUndeclarable> {
    let definitions = settings_type_schema["$defs"].as_object();
    let required_names: Vec<&str> = settings_type_schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut fields = settings_type_schema["properties"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(setting_name, property_schema)| {
            let value_schema = the_value_schema_of_the_property(property_schema, definitions);
            let field_type = the_field_type_of(value_schema).ok_or_else(|| {
                RelaySettingFieldTypeUndeclarable {
                    setting_name: setting_name.clone(),
                    property_schema: property_schema.to_string(),
                }
            })?;
            Ok(RelayAdapterSettingField {
                name: setting_name.clone(),
                field_type,
                secret: value_schema[RELAY_SETTING_SECRET_SCHEMA_KEYWORD] == Value::Bool(true),
                required: required_names.contains(&setting_name.as_str()),
                doc: property_schema["description"]
                    .as_str()
                    .or_else(|| value_schema["description"].as_str())
                    .unwrap_or_default()
                    .to_string(),
                default: property_schema
                    .get("default")
                    .filter(|default| !default.is_null())
                    .cloned(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    fields.sort_by(|field, other_field| field.name.cmp(&other_field.name));
    Ok(fields)
}

/// The schema a `$ref` that resolves to nothing stands for.
static NO_SCHEMA: Value = Value::Null;

/// The schema of the value itself: a `$ref` resolved against `definitions`,
/// and the wrapper schemars writes around a documented reference
/// (`allOf: [x]`) or an `Option` of one (`anyOf: [x, {"type": "null"}]`)
/// taken off.
fn the_value_schema_of_the_property<'schema>(
    property_schema: &'schema Value,
    definitions: Option<&'schema Map<String, Value>>,
) -> &'schema Value {
    if let Some(reference) = property_schema["$ref"].as_str() {
        let definition_name = reference.rsplit('/').next().unwrap_or_default();
        return definitions
            .and_then(|definitions| definitions.get(definition_name))
            .map_or(&NO_SCHEMA, |definition| {
                the_value_schema_of_the_property(definition, definitions)
            });
    }
    for wrapper_keyword in ["allOf", "anyOf"] {
        if let Some(value_schema) =
            property_schema[wrapper_keyword]
                .as_array()
                .and_then(|wrapped| {
                    the_only_item(
                        wrapped
                            .iter()
                            .filter(|subschema| subschema["type"] != "null"),
                    )
                })
        {
            return the_value_schema_of_the_property(value_schema, definitions);
        }
    }
    property_schema
}

fn the_field_type_of(value_schema: &Value) -> Option<RelaySettingFieldType> {
    if let Some(declared) = value_schema.get(RELAY_SETTING_FIELD_TYPE_SCHEMA_KEYWORD) {
        return RelaySettingFieldType::deserialize(declared).ok();
    }
    if let Some(variants) = value_schema["enum"].as_array() {
        return the_select_of_the_variants(variants.iter());
    }
    if let Some(variant_groups) = value_schema["oneOf"].as_array() {
        let variants = variant_groups
            .iter()
            .map(|variant_group| variant_group["enum"].as_array())
            .collect::<Option<Vec<_>>>()?;
        return the_select_of_the_variants(variants.into_iter().flatten());
    }
    match the_one_non_null_instance_type(value_schema)? {
        "string" => Some(RelaySettingFieldType::String),
        "integer" | "number" => Some(RelaySettingFieldType::Number),
        "boolean" => Some(RelaySettingFieldType::Boolean),
        "array" if the_one_non_null_instance_type(&value_schema["items"]) == Some("string") => {
            Some(RelaySettingFieldType::StringList)
        }
        _ => None,
    }
}

/// A `select` of a unit-variant enum's values, or `None` when one is not a
/// string. schemars writes the undocumented variants as one `enum` and each
/// documented one as its own `{"enum": [<value>]}` in a `oneOf`.
fn the_select_of_the_variants<'schema>(
    variants: impl Iterator<Item = &'schema Value>,
) -> Option<RelaySettingFieldType> {
    let options = variants
        .filter(|variant| !variant.is_null())
        .map(|variant| variant.as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()?;
    Some(RelaySettingFieldType::Select { options })
}

/// The schema's `type`, with the `null` an `Option` adds left out.
fn the_one_non_null_instance_type(value_schema: &Value) -> Option<&str> {
    match &value_schema["type"] {
        Value::String(instance_type) => Some(instance_type),
        Value::Array(instance_types) => the_only_item(
            instance_types
                .iter()
                .filter_map(Value::as_str)
                .filter(|instance_type| *instance_type != "null"),
        ),
        _ => None,
    }
}

/// The one item `items` yields, or `None` when it yields none or several.
fn the_only_item<I: Iterator>(mut items: I) -> Option<I::Item> {
    let only_item = items.next()?;
    items.next().is_none().then_some(only_item)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        RelaySecretEnteredAsFile, RelaySecretEnteredAsString, RelaySecretEnteredAsUrl,
        RelaySecretSetting,
    };

    #[derive(Debug, Deserialize, schemars::JsonSchema)]
    #[serde(rename_all = "snake_case")]
    #[allow(dead_code)]
    enum ProviderRegion {
        Us,
        Eu,
    }

    #[derive(Debug, Deserialize, schemars::JsonSchema)]
    #[serde(rename_all = "snake_case")]
    #[allow(dead_code)]
    enum ProviderSupportLevel {
        Community,
        Standard,
        /// Answered within the hour.
        Priority,
    }

    #[derive(Debug, Deserialize, schemars::JsonSchema)]
    #[serde(rename_all = "snake_case")]
    #[allow(dead_code)]
    enum ProviderTier {
        /// The tier with no charge.
        Free,
        /// The tier a plan pays for.
        Paid,
    }

    /// A provider's settings, one of each field type.
    #[derive(Debug, Deserialize, schemars::JsonSchema)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct ProviderSettingsOfEveryFieldType {
        /// The project's name.
        project_name: String,
        /// The relay's URL, its token in the path.
        relay_url_carrying_a_token: RelaySecretSetting<RelaySecretEnteredAsUrl>,
        /// The project's signing key file.
        #[serde(default)]
        signing_key: Option<RelaySecretSetting<RelaySecretEnteredAsFile>>,
        /// The provider's API key.
        #[serde(default)]
        api_key: Option<RelaySecretSetting<RelaySecretEnteredAsString>>,
        /// The port it listens on.
        #[serde(default)]
        port: Option<u16>,
        /// Whether it records.
        #[serde(default)]
        records: bool,
        /// The region it runs in.
        region: ProviderRegion,
        /// The region it falls back to.
        #[serde(default)]
        fallback_region: Option<ProviderRegion>,
        /// The tags it carries.
        #[serde(default)]
        tags: Vec<String>,
        /// The tier it is billed at.
        tier: ProviderTier,
        /// The support it is answered with.
        support_level: ProviderSupportLevel,
    }

    fn the_field_named<'f>(
        fields: &'f [RelayAdapterSettingField],
        name: &str,
    ) -> &'f RelayAdapterSettingField {
        fields
            .iter()
            .find(|field| field.name == name)
            .unwrap_or_else(|| panic!("no field `{name}` in {fields:#?}"))
    }

    #[test]
    fn each_rust_type_is_listed_as_its_field_type() {
        let fields = relay_adapter_setting_fields_of::<ProviderSettingsOfEveryFieldType>();
        let region_options = RelaySettingFieldType::Select {
            options: vec!["us".to_string(), "eu".to_string()],
        };
        for (name, field_type) in [
            ("project_name", RelaySettingFieldType::String),
            ("relay_url_carrying_a_token", RelaySettingFieldType::Url),
            ("signing_key", RelaySettingFieldType::File),
            ("api_key", RelaySettingFieldType::String),
            ("port", RelaySettingFieldType::Number),
            ("records", RelaySettingFieldType::Boolean),
            ("region", region_options.clone()),
            ("fallback_region", region_options),
            ("tags", RelaySettingFieldType::StringList),
            (
                "support_level",
                RelaySettingFieldType::Select {
                    options: vec![
                        "community".to_string(),
                        "standard".to_string(),
                        "priority".to_string(),
                    ],
                },
            ),
            (
                "tier",
                RelaySettingFieldType::Select {
                    options: vec!["free".to_string(), "paid".to_string()],
                },
            ),
        ] {
            assert_eq!(
                the_field_named(&fields, name).field_type,
                field_type,
                "{name}"
            );
        }
    }

    #[test]
    fn the_fields_are_listed_in_name_order_whatever_order_the_schema_holds_them_in() {
        let fields = relay_adapter_setting_fields_in_the_schema(&serde_json::json!({
            "type": "object",
            "properties": {
                "zeta": {"type": "string"},
                "alpha": {"type": "string"},
            },
        }))
        .unwrap();
        assert_eq!(
            fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "zeta"]
        );
    }

    #[test]
    fn a_secret_is_flagged_whatever_its_field_type_and_nothing_else_is() {
        let fields = relay_adapter_setting_fields_of::<ProviderSettingsOfEveryFieldType>();
        let mut secret_names: Vec<&str> = fields
            .iter()
            .filter(|field| field.secret)
            .map(|field| field.name.as_str())
            .collect();
        secret_names.sort();
        assert_eq!(
            secret_names,
            ["api_key", "relay_url_carrying_a_token", "signing_key"]
        );
    }

    #[test]
    fn a_field_is_required_unless_it_is_optional_or_defaulted() {
        let fields = relay_adapter_setting_fields_of::<ProviderSettingsOfEveryFieldType>();
        let mut required_names: Vec<&str> = fields
            .iter()
            .filter(|field| field.required)
            .map(|field| field.name.as_str())
            .collect();
        required_names.sort();
        assert_eq!(
            required_names,
            [
                "project_name",
                "region",
                "relay_url_carrying_a_token",
                "support_level",
                "tier"
            ]
        );
    }

    #[test]
    fn a_defaulted_field_carries_its_rust_default_and_no_other_field_carries_one() {
        let fields = relay_adapter_setting_fields_of::<ProviderSettingsOfEveryFieldType>();
        assert_eq!(
            the_field_named(&fields, "records").default,
            Some(serde_json::json!(false))
        );
        assert_eq!(
            the_field_named(&fields, "tags").default,
            Some(serde_json::json!([]))
        );
        for name in ["project_name", "port", "signing_key", "region"] {
            assert_eq!(the_field_named(&fields, name).default, None, "{name}");
        }
    }

    #[test]
    fn each_field_carries_its_doc_comment() {
        let fields = relay_adapter_setting_fields_of::<ProviderSettingsOfEveryFieldType>();
        assert_eq!(
            the_field_named(&fields, "relay_url_carrying_a_token").doc,
            "The relay's URL, its token in the path."
        );
        assert_eq!(
            the_field_named(&fields, "fallback_region").doc,
            "The region it falls back to."
        );
    }

    #[test]
    fn a_field_serializes_flat_with_a_selects_options_beside_its_type() {
        let fields = relay_adapter_setting_fields_of::<ProviderSettingsOfEveryFieldType>();
        assert_eq!(
            serde_json::to_value(the_field_named(&fields, "region")).unwrap(),
            serde_json::json!({
                "name": "region",
                "type": "select",
                "options": ["us", "eu"],
                "secret": false,
                "required": true,
                "doc": "The region it runs in.",
            })
        );
        assert_eq!(
            serde_json::to_value(the_field_named(&fields, "signing_key")).unwrap(),
            serde_json::json!({
                "name": "signing_key",
                "type": "file",
                "secret": true,
                "required": false,
                "doc": "The project's signing key file.",
            })
        );
    }

    #[test]
    fn a_field_of_a_type_no_field_type_covers_is_refused_naming_it() {
        #[derive(Deserialize, schemars::JsonSchema)]
        #[allow(dead_code)]
        struct SettingsWithANestedObject {
            limits: std::collections::BTreeMap<String, u32>,
        }
        let undeclarable = relay_adapter_setting_fields_in_the_schema(
            &SettingsWithANestedObject::processor_config_schema_document(),
        )
        .unwrap_err();
        assert_eq!(undeclarable.setting_name, "limits");
    }
}
