// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The runtime client's node UI manifest files, generated from the contract's `NodeUiManifest`
//! component: `node_ui_manifest.d.ts`, which names its types, and
//! `node_ui_manifest.schema.json`, the self-contained JSON Schema an author's editor reads.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

use crate::generate_runtime_client::{COMPONENT_SCHEMA_REFERENCE_PREFIX, GENERATED_SOURCE_HEADER};

pub(crate) const NODE_UI_MANIFEST_TYPES_FILE_NAME: &str = "node_ui_manifest.d.ts";
pub(crate) const NODE_UI_MANIFEST_JSON_SCHEMA_FILE_NAME: &str = "node_ui_manifest.schema.json";

/// The component that states the manifest.
const NODE_UI_MANIFEST_COMPONENT_NAME: &str = "NodeUiManifest";

/// Where a definition sits in the self-contained JSON Schema, before its name.
const JSON_SCHEMA_DEFINITION_REFERENCE_PREFIX: &str = "#/$defs/";

/// The JSON Schema dialect the contract's schemas are written in.
const JSON_SCHEMA_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

/// `node_ui_manifest.d.ts`: the manifest's type and every type it names, re-exported from the
/// contract's types so each is declared once.
pub(crate) fn node_ui_manifest_declarations(
    component_schemas: &Map<String, Value>,
) -> Result<String> {
    let names = the_manifest_and_every_component_it_names(component_schemas)?;
    let mut declarations = String::from(GENERATED_SOURCE_HEADER);
    declarations.push_str(
        "\n/**\n * What a module's `node_ui/tatolab_node_ui.json` registers, and every type it names.\n * Its JSON Schema is `node_ui_manifest.schema.json`.\n */\nexport type {\n",
    );
    for name in &names {
        let _ = writeln!(declarations, "  {name},");
    }
    declarations.push_str("} from \"./control_contract.js\";\n");
    Ok(declarations)
}

/// `node_ui_manifest.schema.json`: the manifest's schema with every component it names under
/// `$defs`, descriptions kept, so it reads with no other file.
pub(crate) fn node_ui_manifest_json_schema(
    component_schemas: &Map<String, Value>,
) -> Result<String> {
    let names = the_manifest_and_every_component_it_names(component_schemas)?;
    let mut definitions = Map::new();
    for name in names
        .iter()
        .filter(|name| *name != NODE_UI_MANIFEST_COMPONENT_NAME)
    {
        definitions.insert(
            name.clone(),
            with_references_into_the_definitions(&component_schemas[name.as_str()]),
        );
    }
    let mut schema = Map::new();
    schema.insert("$schema".into(), json!(JSON_SCHEMA_DIALECT));
    schema.insert("title".into(), json!("tatolab_node_ui.json"));
    let Value::Object(manifest) =
        with_references_into_the_definitions(&component_schemas[NODE_UI_MANIFEST_COMPONENT_NAME])
    else {
        bail!("the contract's `{NODE_UI_MANIFEST_COMPONENT_NAME}` is not a schema object");
    };
    schema.extend(manifest);
    if !definitions.is_empty() {
        schema.insert("$defs".into(), Value::Object(definitions));
    }
    let mut written = serde_json::to_string_pretty(&Value::Object(schema))
        .context("writing the node UI manifest's JSON Schema")?;
    written.push('\n');
    Ok(written)
}

/// `NodeUiManifest` and every component its schema reaches, in the contract's order.
fn the_manifest_and_every_component_it_names(
    component_schemas: &Map<String, Value>,
) -> Result<Vec<String>> {
    if !component_schemas.contains_key(NODE_UI_MANIFEST_COMPONENT_NAME) {
        bail!("the contract file has no `{NODE_UI_MANIFEST_COMPONENT_NAME}` component schema");
    }
    let mut reached = BTreeSet::from([NODE_UI_MANIFEST_COMPONENT_NAME.to_owned()]);
    let mut to_visit = vec![NODE_UI_MANIFEST_COMPONENT_NAME.to_owned()];
    while let Some(name) = to_visit.pop() {
        let mut named = Vec::new();
        every_component_named_in(&component_schemas[name.as_str()], &mut named);
        for named_name in named {
            if !component_schemas.contains_key(&named_name) {
                bail!("`{name}` names `{named_name}`, which the contract file has no schema for");
            }
            if reached.insert(named_name.clone()) {
                to_visit.push(named_name);
            }
        }
    }
    Ok(component_schemas
        .keys()
        .filter(|name| reached.contains(*name))
        .cloned()
        .collect())
}

fn every_component_named_in(schema: &Value, named: &mut Vec<String>) {
    match schema {
        Value::Object(object) => {
            for (keyword, inner) in object {
                match (keyword.as_str(), inner) {
                    ("$ref", Value::String(reference)) => {
                        if let Some(name) =
                            reference.strip_prefix(COMPONENT_SCHEMA_REFERENCE_PREFIX)
                        {
                            named.push(name.to_owned());
                        }
                    }
                    ("const" | "default", _) => {}
                    _ => every_component_named_in(inner, named),
                }
            }
        }
        Value::Array(items) => items
            .iter()
            .for_each(|item| every_component_named_in(item, named)),
        _ => {}
    }
}

fn with_references_into_the_definitions(schema: &Value) -> Value {
    match schema {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(keyword, inner)| {
                    let rewritten = match (keyword.as_str(), inner) {
                        ("$ref", Value::String(reference)) => {
                            match reference.strip_prefix(COMPONENT_SCHEMA_REFERENCE_PREFIX) {
                                Some(name) => {
                                    json!(format!(
                                        "{JSON_SCHEMA_DEFINITION_REFERENCE_PREFIX}{name}"
                                    ))
                                }
                                None => inner.clone(),
                            }
                        }
                        ("const" | "default", _) => inner.clone(),
                        _ => with_references_into_the_definitions(inner),
                    };
                    (keyword.clone(), rewritten)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(with_references_into_the_definitions)
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests;
