// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The built-in pin types as the committed control contract file lists them in `x-pin-types`,
//! read once for every generator that writes them into another language.

use std::collections::BTreeSet;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value};

use crate::generate_runtime_client::COMPONENT_SCHEMA_REFERENCE_PREFIX;

/// The component every built-in pin type is an alternative of, tagged on `type`.
pub(crate) const PIN_TYPE_COMPONENT_NAME: &str = "PinType";

/// The component whose keys are the annotations a pin type row names.
pub(crate) const PIN_ANNOTATIONS_COMPONENT_NAME: &str = "PinAnnotations";

/// The key a `PinType` alternative is tagged on.
const PIN_TYPE_TAG_KEY: &str = "type";

/// One row of `x-pin-types`.
pub(crate) struct BuiltInPinTypeRow<'contract> {
    pub(crate) name: &'contract str,
    pub(crate) label: &'contract str,
    pub(crate) documentation: &'contract str,
    pub(crate) flows: Vec<&'contract str>,
    pub(crate) annotations: Vec<&'contract str>,
    pub(crate) python_forms: Vec<&'contract str>,
    pub(crate) typescript_form: &'contract str,
    /// The component that states its stored value; `None` when it stores none.
    pub(crate) value_component: Option<&'contract str>,
    pub(crate) since_contract_version: u64,
    pub(crate) stability: &'contract str,
}

/// Every row of `contract`'s `x-pin-types`, in its order. Refused when the rows name other
/// types than `PinType`'s alternatives, in another order, or an annotation `PinAnnotations` lacks.
pub(crate) fn built_in_pin_type_rows(contract: &Value) -> Result<Vec<BuiltInPinTypeRow<'_>>> {
    let component_schemas = component_schemas_of(contract)?;
    let rows: Vec<BuiltInPinTypeRow<'_>> = contract["x-pin-types"]
        .as_array()
        .context("the contract file has no `x-pin-types` array")?
        .iter()
        .map(BuiltInPinTypeRow::of)
        .collect::<Result<_>>()?;
    let pin_type_schema = component_schemas
        .get(PIN_TYPE_COMPONENT_NAME)
        .with_context(|| {
            format!("the contract file has no `{PIN_TYPE_COMPONENT_NAME}` component")
        })?;
    let tags = tags_of_a_tagged_union(pin_type_schema, PIN_TYPE_TAG_KEY)
        .with_context(|| format!("`{PIN_TYPE_COMPONENT_NAME}`"))?;
    let row_names: Vec<&str> = rows.iter().map(|row| row.name).collect();
    ensure!(
        row_names == tags,
        "`x-pin-types` names {row_names:?}, but `{PIN_TYPE_COMPONENT_NAME}`'s alternatives are \
         {tags:?}; the two must name the same types in the same order"
    );
    let annotation_names = property_names_of(component_schemas, PIN_ANNOTATIONS_COMPONENT_NAME)?;
    for row in &rows {
        if let Some(unknown) = row
            .annotations
            .iter()
            .find(|annotation| !annotation_names.contains(annotation))
        {
            bail!(
                "the `{}` row names the annotation `{unknown}`, which \
                 `{PIN_ANNOTATIONS_COMPONENT_NAME}` has no key for",
                row.name
            );
        }
        if let Some(value_component) = row.value_component {
            ensure!(
                component_schemas.contains_key(value_component),
                "the `{}` row's value schema names `{value_component}`, which is not a component",
                row.name
            );
        }
    }
    Ok(rows)
}

impl<'contract> BuiltInPinTypeRow<'contract> {
    fn of(row: &'contract Value) -> Result<Self> {
        let name = row["name"]
            .as_str()
            .context("an `x-pin-types` row has no `name`")?;
        let text = |key: &str| -> Result<&'contract str> {
            row[key]
                .as_str()
                .with_context(|| format!("the `{name}` row has no `{key}`"))
        };
        let texts = |key: &str| -> Result<Vec<&'contract str>> {
            row[key]
                .as_array()
                .with_context(|| format!("the `{name}` row has no `{key}` array"))?
                .iter()
                .map(|item| {
                    item.as_str()
                        .with_context(|| format!("the `{name}` row's `{key}` holds a non-string"))
                })
                .collect()
        };
        let value_component = match row.get("value_json_schema") {
            None => None,
            Some(value_schema) => Some(component_name_of_a_reference(value_schema).with_context(
                || format!("the `{name}` row's `value_json_schema` is not a component reference"),
            )?),
        };
        Ok(Self {
            name,
            label: text("label")?,
            documentation: text("documentation")?,
            flows: texts("flows")?,
            annotations: texts("annotations")?,
            python_forms: texts("python_forms")?,
            typescript_form: text("typescript_form")?,
            value_component,
            since_contract_version: row["x-since"]
                .as_u64()
                .with_context(|| format!("the `{name}` row has no `x-since`"))?,
            stability: text("x-stability")?,
        })
    }
}

pub(crate) fn component_schemas_of(contract: &Value) -> Result<&Map<String, Value>> {
    contract["components"]["schemas"]
        .as_object()
        .context("the contract file has no `components.schemas`")
}

/// The component a `{ "$ref": "#/components/schemas/<name>" }` schema names.
pub(crate) fn component_name_of_a_reference(schema: &Value) -> Option<&str> {
    schema["$ref"]
        .as_str()?
        .strip_prefix(COMPONENT_SCHEMA_REFERENCE_PREFIX)
}

/// The property names of the object component `name`.
pub(crate) fn property_names_of<'contract>(
    component_schemas: &'contract Map<String, Value>,
    name: &str,
) -> Result<Vec<&'contract str>> {
    Ok(component_schemas
        .get(name)
        .and_then(|schema| schema["properties"].as_object())
        .with_context(|| format!("the contract file has no object component `{name}`"))?
        .keys()
        .map(String::as_str)
        .collect())
}

/// The tag values of a `oneOf` of objects each tagged by a `const` under `tag_key`, in order.
pub(crate) fn tags_of_a_tagged_union<'schema>(
    schema: &'schema Value,
    tag_key: &str,
) -> Result<Vec<&'schema str>> {
    schema["oneOf"]
        .as_array()
        .context("is not a `oneOf`")?
        .iter()
        .map(|alternative| {
            alternative["properties"][tag_key]["const"]
                .as_str()
                .with_context(|| format!("has an alternative with no `{tag_key}` tag"))
        })
        .collect()
}

/// `roots` and every component they reach through `$ref`, in the order `component_schemas`
/// holds them.
pub(crate) fn components_reached_from<'contract>(
    component_schemas: &'contract Map<String, Value>,
    roots: &[&str],
) -> Result<Vec<&'contract str>> {
    let mut reached = BTreeSet::new();
    let mut unvisited: Vec<String> = roots.iter().map(|root| (*root).to_owned()).collect();
    while let Some(name) = unvisited.pop() {
        if !reached.insert(name.clone()) {
            continue;
        }
        let schema = component_schemas
            .get(&name)
            .with_context(|| format!("`{name}` is referred to but is not a component"))?;
        collect_referenced_components(schema, &mut unvisited);
    }
    Ok(component_schemas
        .keys()
        .map(String::as_str)
        .filter(|name| reached.contains(*name))
        .collect())
}

fn collect_referenced_components(schema: &Value, referenced: &mut Vec<String>) {
    match schema {
        Value::Object(members) => {
            if let Some(name) = component_name_of_a_reference(schema) {
                referenced.push(name.to_owned());
            }
            for member in members.values() {
                collect_referenced_components(member, referenced);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_referenced_components(item, referenced);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests;
