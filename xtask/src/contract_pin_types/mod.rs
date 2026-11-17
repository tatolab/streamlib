// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The built-in pin types as the committed control contract file lists them in `x-pin-types`,
//! read once for every generator that writes them into another language.

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

use crate::contract_component_references::{
    component_name_of_a_reference, component_schemas_of, property_names_of,
    string_values_of_a_closed_set, tags_of_a_tagged_union,
};

/// The component every built-in pin type is an alternative of, tagged on `type`.
pub(crate) const PIN_TYPE_COMPONENT_NAME: &str = "PinType";

/// The component whose keys are the annotations a pin type row names.
pub(crate) const PIN_ANNOTATIONS_COMPONENT_NAME: &str = "PinAnnotations";

/// The component whose values are the built-in pin type names.
pub(crate) const BUILT_IN_PIN_TYPE_NAME_COMPONENT_NAME: &str = "BuiltInPinTypeName";

/// What a row's `x-stability` may say.
pub(crate) const PIN_TYPE_STABILITIES: &[&str] = &["stable", "experimental"];

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
/// types than `PinType`'s alternatives or `BuiltInPinTypeName`'s values, or in another order;
/// or when a row names an annotation `PinAnnotations` lacks, a value schema that is not a
/// component, or a stability other than `stable` and `experimental`.
pub(crate) fn built_in_pin_type_rows(contract: &Value) -> Result<Vec<BuiltInPinTypeRow<'_>>> {
    let component_schemas = component_schemas_of(contract)?;
    let rows: Vec<BuiltInPinTypeRow<'_>> = contract["x-pin-types"]
        .as_array()
        .context("the contract file has no `x-pin-types` array")?
        .iter()
        .map(BuiltInPinTypeRow::of)
        .collect::<Result<_>>()?;
    let component = |name: &str| {
        component_schemas
            .get(name)
            .with_context(|| format!("the contract file has no `{name}` component"))
    };
    let row_names: Vec<&str> = rows.iter().map(|row| row.name).collect();
    let tags = tags_of_a_tagged_union(component(PIN_TYPE_COMPONENT_NAME)?, PIN_TYPE_TAG_KEY)
        .with_context(|| format!("`{PIN_TYPE_COMPONENT_NAME}`"))?;
    ensure!(
        row_names == tags,
        "`x-pin-types` names {row_names:?}, but `{PIN_TYPE_COMPONENT_NAME}`'s alternatives are \
         {tags:?}; the two must name the same types in the same order"
    );
    let names = string_values_of_a_closed_set(component(BUILT_IN_PIN_TYPE_NAME_COMPONENT_NAME)?)
        .with_context(|| format!("`{BUILT_IN_PIN_TYPE_NAME_COMPONENT_NAME}`"))?;
    ensure!(
        row_names == names,
        "`x-pin-types` names {row_names:?}, but `{BUILT_IN_PIN_TYPE_NAME_COMPONENT_NAME}` holds \
         {names:?}; the two must name the same types in the same order"
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
        ensure!(
            PIN_TYPE_STABILITIES.contains(&row.stability),
            "the `{}` row's stability is `{}`, neither of {PIN_TYPE_STABILITIES:?}",
            row.name,
            row.stability
        );
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

#[cfg(test)]
mod tests;
