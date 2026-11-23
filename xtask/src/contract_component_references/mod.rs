// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! How one component schema of the control contract names another: a `$ref` to
//! `#/components/schemas/<name>`. Every generator that follows those references reads them here.

use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

/// Where a component sits in the contract file, before its name.
pub(crate) const COMPONENT_SCHEMA_REFERENCE_PREFIX: &str = "#/components/schemas/";

/// The component names `schema` refers to, in the order written, a name once for each
/// reference. What a `const` or a `default` holds is a value, not a schema, so a `$ref` inside
/// one names nothing.
pub(crate) fn component_names_a_schema_names(schema: &Value) -> Vec<&str> {
    let mut named = Vec::new();
    add_the_component_names_a_schema_names(schema, &mut named);
    named
}

/// `roots` and every component their schemas reach, through any number of references, in the
/// contract's order. A root or a reference the contract has no schema for is refused.
pub(crate) fn component_names_reached_from<'contract>(
    component_schemas: &'contract Map<String, Value>,
    roots: &[&str],
) -> Result<Vec<&'contract str>> {
    let mut reached = BTreeSet::new();
    let mut to_visit = Vec::new();
    for root in roots {
        let Some((name, _)) = component_schemas.get_key_value(*root) else {
            bail!("the contract file has no `{root}` component schema");
        };
        if reached.insert(name.as_str()) {
            to_visit.push(name.as_str());
        }
    }
    while let Some(name) = to_visit.pop() {
        for named in component_names_a_schema_names(&component_schemas[name]) {
            let Some((named, _)) = component_schemas.get_key_value(named) else {
                bail!("`{name}` names `{named}`, which the contract file has no schema for");
            };
            if reached.insert(named.as_str()) {
                to_visit.push(named.as_str());
            }
        }
    }
    Ok(component_schemas
        .keys()
        .map(String::as_str)
        .filter(|name| reached.contains(name))
        .collect())
}

/// The contract's `components.schemas`.
pub(crate) fn component_schemas_of(contract: &Value) -> Result<&Map<String, Value>> {
    contract["components"]["schemas"]
        .as_object()
        .context("the contract file has no `components.schemas`")
}

/// The component a `{ "$ref": "#/components/schemas/<name>" }` schema names, when it is one.
pub(crate) fn component_name_of_a_reference(schema: &Value) -> Option<&str> {
    schema["$ref"]
        .as_str()?
        .strip_prefix(COMPONENT_SCHEMA_REFERENCE_PREFIX)
}

/// The keywords by which a schema states what type it is.
const KEYWORDS_THAT_STATE_A_TYPE: &[&str] =
    &["type", "$ref", "const", "properties", "oneOf", "anyOf"];

/// The alternatives of the `oneOf` or `anyOf` `schema` states as its type. A union none of whose
/// alternatives states a type only narrows the values of the type the schema states around it,
/// as each alternative of a bundle's `members` narrows what its items may be, so it is no type of
/// its own and none is returned.
pub(crate) fn the_union_a_schema_is(schema: &Value) -> Option<&Vec<Value>> {
    ["oneOf", "anyOf"]
        .into_iter()
        .find_map(|union_keyword| schema[union_keyword].as_array())
        .filter(|alternatives| {
            alternatives.iter().any(|alternative| {
                KEYWORDS_THAT_STATE_A_TYPE
                    .iter()
                    .any(|keyword| alternative.get(keyword).is_some())
            })
        })
}

/// The property names of the object component `name`, in order.
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

/// The values of a `oneOf` of string `const`s, in order.
pub(crate) fn string_values_of_a_closed_set(schema: &Value) -> Result<Vec<&str>> {
    schema["oneOf"]
        .as_array()
        .context("is not a `oneOf`")?
        .iter()
        .map(|alternative| {
            alternative["const"]
                .as_str()
                .context("has an alternative that is not a string `const`")
        })
        .collect()
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

fn add_the_component_names_a_schema_names<'schema>(
    schema: &'schema Value,
    named: &mut Vec<&'schema str>,
) {
    match schema {
        Value::Object(members) => {
            for (keyword, member) in members {
                match (keyword.as_str(), member) {
                    ("$ref", Value::String(reference)) => {
                        if let Some(name) =
                            reference.strip_prefix(COMPONENT_SCHEMA_REFERENCE_PREFIX)
                        {
                            named.push(name);
                        }
                    }
                    ("const" | "default", _) => {}
                    _ => add_the_component_names_a_schema_names(member, named),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                add_the_component_names_a_schema_names(item, named);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests;
