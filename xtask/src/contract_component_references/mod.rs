// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! How one component schema of the control contract names another: a `$ref` to
//! `#/components/schemas/<name>`. Every generator that follows those references reads them here.

use std::collections::BTreeSet;

use anyhow::{Result, bail};
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
