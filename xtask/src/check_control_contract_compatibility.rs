// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `cargo xtask check-control-contract-compatibility`: the CI gate that fails a change to the
//! control contract file which breaks a client built against the earlier file without bumping
//! `x-contract-version`.
//!
//! A break is a stable operation, notification or discovery tool that is gone, a field that is
//! gone, a field a caller must now send, a field a result no longer always carries, or a type or
//! set of values that no longer holds what it held. An addition is no break; it carries the
//! contract version it arrived in as `x-since`. An `experimental` entry may change freely.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use streamlib_control_contract::CONTROL_CONTRACT_FILE_WORKSPACE_PATH;

/// Fail when the workspace's contract file breaks a client of the contract file at
/// `earlier_contract_file` without a higher contract version.
pub fn run(workspace_root: &Path, earlier_contract_file: &Path) -> Result<()> {
    let earlier_contract = the_contract_file_at(earlier_contract_file)?;
    let later_contract =
        the_contract_file_at(&workspace_root.join(CONTROL_CONTRACT_FILE_WORKSPACE_PATH))?;
    if let Some(refusal) = the_refusal_of_a_contract_change(&earlier_contract, &later_contract) {
        bail!("{refusal}");
    }
    tracing::info!(
        "{CONTROL_CONTRACT_FILE_WORKSPACE_PATH} breaks no client of {}",
        earlier_contract_file.display()
    );
    Ok(())
}

fn the_contract_file_at(contract_file: &Path) -> Result<Value> {
    let contents = std::fs::read_to_string(contract_file)
        .with_context(|| format!("reading {}", contract_file.display()))?;
    serde_json::from_str(&contents).with_context(|| format!("parsing {}", contract_file.display()))
}

/// Why the change from `earlier_contract` to `later_contract` is refused, or `None` when it is
/// compatible or its break is carried by a higher contract version.
pub fn the_refusal_of_a_contract_change(
    earlier_contract: &Value,
    later_contract: &Value,
) -> Option<String> {
    let earlier_version = the_contract_version_of(earlier_contract);
    let later_version = the_contract_version_of(later_contract);
    if later_version < earlier_version {
        return Some(format!(
            "the contract version went down, from {earlier_version} to {later_version}"
        ));
    }
    let misdated_additions = the_additions_dated_outside(
        earlier_contract,
        later_contract,
        earlier_version..=later_version,
    );
    if !misdated_additions.is_empty() {
        return Some(format!(
            "an added entry carries the contract version it arrived in as `x-since`, which is \
             {earlier_version} to {later_version} here:\n{}",
            misdated_additions.join("\n")
        ));
    }
    let breaking_differences = the_breaking_differences_between(earlier_contract, later_contract);
    if breaking_differences.is_empty() || later_version > earlier_version {
        return None;
    }
    Some(format!(
        "{CONTROL_CONTRACT_FILE_WORKSPACE_PATH} breaks a client built against contract version \
         {earlier_version} and still states that version. Bump `CONTROL_CONTRACT_VERSION`, or \
         keep what the client relies on:\n{}",
        breaking_differences.join("\n")
    ))
}

fn the_contract_version_of(contract: &Value) -> u64 {
    contract["info"]["x-contract-version"]
        .as_u64()
        .unwrap_or_default()
}

const ENTRY_LISTS: &[&str] = &["methods", "x-notifications", "x-agent-discovery-tools"];

fn the_entries_of<'contract>(contract: &'contract Value, entry_list: &str) -> &'contract [Value] {
    contract[entry_list]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn the_entry_named<'contract>(
    entries: &'contract [Value],
    name: &Value,
) -> Option<&'contract Value> {
    entries.iter().find(|entry| &entry["name"] == name)
}

fn the_additions_dated_outside(
    earlier_contract: &Value,
    later_contract: &Value,
    contract_versions_of_the_change: std::ops::RangeInclusive<u64>,
) -> Vec<String> {
    let mut misdated = Vec::new();
    for entry_list in ENTRY_LISTS {
        let earlier_entries = the_entries_of(earlier_contract, entry_list);
        for added in the_entries_of(later_contract, entry_list)
            .iter()
            .filter(|entry| the_entry_named(earlier_entries, &entry["name"]).is_none())
        {
            let Some(since) = added.get("x-since") else {
                continue;
            };
            if !since
                .as_u64()
                .is_some_and(|since| contract_versions_of_the_change.contains(&since))
            {
                misdated.push(format!("- `{}` states `x-since` {since}", added["name"]));
            }
        }
    }
    misdated
}

/// Which side of the wire a schema's values travel from.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SchemaDirection {
    /// A client sends it: params.
    SentByAClient,
    /// A client reads it: a result, or a notification's params.
    ReadByAClient,
}

/// Every difference from `earlier_contract` to `later_contract` that breaks a client built
/// against the earlier one, one line each.
pub fn the_breaking_differences_between(
    earlier_contract: &Value,
    later_contract: &Value,
) -> Vec<String> {
    let mut comparison = ContractComparison {
        earlier_contract,
        later_contract,
        schema_pairs_compared: BTreeSet::new(),
        breaking_differences: Vec::new(),
    };
    for entry_list in ENTRY_LISTS {
        let later_entries = the_entries_of(later_contract, entry_list);
        for earlier_entry in the_entries_of(earlier_contract, entry_list) {
            if earlier_entry["x-stability"] == "experimental" {
                continue;
            }
            let name = earlier_entry["name"].as_str().unwrap_or_default();
            let Some(later_entry) = the_entry_named(later_entries, &earlier_entry["name"]) else {
                comparison
                    .breaking_differences
                    .push(format!("- `{name}` was removed from `{entry_list}`"));
                continue;
            };
            let params_direction = if *entry_list == "x-notifications" {
                SchemaDirection::ReadByAClient
            } else {
                SchemaDirection::SentByAClient
            };
            comparison.compare_schemas(
                &format!("`{name}` params"),
                the_params_schema_of(earlier_entry),
                the_params_schema_of(later_entry),
                params_direction,
            );
            comparison.compare_schemas(
                &format!("`{name}` result"),
                the_result_schema_of(earlier_entry),
                the_result_schema_of(later_entry),
                SchemaDirection::ReadByAClient,
            );
        }
    }
    comparison.breaking_differences
}

/// An operation's params are one by-name `params` entry; a discovery tool's are the schema itself.
fn the_params_schema_of(entry: &Value) -> &Value {
    match &entry["params"] {
        Value::Array(params) => params
            .first()
            .map_or(&Value::Null, |param| &param["schema"]),
        schema => schema,
    }
}

fn the_result_schema_of(entry: &Value) -> &Value {
    match entry["result"].get("schema") {
        Some(schema) => schema,
        None => &entry["result"],
    }
}

struct ContractComparison<'contracts> {
    earlier_contract: &'contracts Value,
    later_contract: &'contracts Value,
    schema_pairs_compared: BTreeSet<(String, String, SchemaDirection)>,
    breaking_differences: Vec<String>,
}

impl<'contracts> ContractComparison<'contracts> {
    fn compare_schemas(
        &mut self,
        path: &str,
        earlier_schema: &'contracts Value,
        later_schema: &'contracts Value,
        direction: SchemaDirection,
    ) {
        if let (Some(earlier_reference), Some(later_reference)) = (
            earlier_schema["$ref"].as_str(),
            later_schema["$ref"].as_str(),
        ) && !self.schema_pairs_compared.insert((
            earlier_reference.to_owned(),
            later_reference.to_owned(),
            direction,
        )) {
            return;
        }
        let earlier_schema = the_schema_a_reference_names(self.earlier_contract, earlier_schema);
        let later_schema = the_schema_a_reference_names(self.later_contract, later_schema);
        let (Some(earlier_schema), Some(later_schema)) =
            (earlier_schema.as_object(), later_schema.as_object())
        else {
            return;
        };

        let earlier_types = the_values_listed_under(earlier_schema.get("type"));
        let later_types = the_values_listed_under(later_schema.get("type"));
        if !earlier_types.is_empty() && !later_types.is_empty() {
            let (must_hold, held_in, what_changed) = match direction {
                SchemaDirection::SentByAClient => (&earlier_types, &later_types, "no longer takes"),
                SchemaDirection::ReadByAClient => (&later_types, &earlier_types, "may now be"),
            };
            for outside in must_hold.difference(held_in) {
                self.breaking_differences
                    .push(format!("- {path} {what_changed} the type {outside}"));
            }
        }
        if direction == SchemaDirection::SentByAClient {
            let later_values = the_values_listed_under(later_schema.get("enum"));
            if !later_values.is_empty() {
                for removed in
                    the_values_listed_under(earlier_schema.get("enum")).difference(&later_values)
                {
                    self.breaking_differences
                        .push(format!("- {path} no longer takes the value {removed}"));
                }
            }
        }
        if let (Some(earlier_const), Some(later_const)) =
            (earlier_schema.get("const"), later_schema.get("const"))
            && earlier_const != later_const
        {
            self.breaking_differences.push(format!(
                "- {path} changed from the value {earlier_const} to {later_const}"
            ));
        }

        let earlier_required = the_values_listed_under(earlier_schema.get("required"));
        let later_required = the_values_listed_under(later_schema.get("required"));
        match direction {
            SchemaDirection::SentByAClient => {
                for now_required in later_required.difference(&earlier_required) {
                    self.breaking_differences
                        .push(format!("- {path} now requires the field {now_required}"));
                }
            }
            SchemaDirection::ReadByAClient => {
                for no_longer_required in earlier_required.difference(&later_required) {
                    self.breaking_differences.push(format!(
                        "- {path} no longer always carries the field {no_longer_required}"
                    ));
                }
            }
        }

        let later_properties = later_schema.get("properties").and_then(Value::as_object);
        for (field, earlier_field_schema) in earlier_schema
            .get("properties")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            match later_properties.and_then(|properties| properties.get(field)) {
                Some(later_field_schema) => self.compare_schemas(
                    &format!("{path}.{field}"),
                    earlier_field_schema,
                    later_field_schema,
                    direction,
                ),
                None => self
                    .breaking_differences
                    .push(format!("- {path} lost the field \"{field}\"")),
            }
        }

        for nested in ["items", "additionalProperties"] {
            if let (Some(earlier_nested), Some(later_nested)) =
                (earlier_schema.get(nested), later_schema.get(nested))
            {
                self.compare_schemas(
                    &format!("{path}.{nested}"),
                    earlier_nested,
                    later_nested,
                    direction,
                );
            }
        }
        for alternatives in ["oneOf", "anyOf"] {
            let (Some(earlier_alternatives), Some(later_alternatives)) = (
                earlier_schema.get(alternatives).and_then(Value::as_array),
                later_schema.get(alternatives).and_then(Value::as_array),
            ) else {
                continue;
            };
            if direction == SchemaDirection::SentByAClient
                && later_alternatives.len() < earlier_alternatives.len()
            {
                self.breaking_differences.push(format!(
                    "- {path} takes {} alternatives where it took {}",
                    later_alternatives.len(),
                    earlier_alternatives.len()
                ));
            }
            for (index, (earlier_alternative, later_alternative)) in earlier_alternatives
                .iter()
                .zip(later_alternatives)
                .enumerate()
            {
                self.compare_schemas(
                    &format!("{path}.{alternatives}[{index}]"),
                    earlier_alternative,
                    later_alternative,
                    direction,
                );
            }
        }
    }
}

/// The schema `schema` is, following a `$ref` into its contract's `components.schemas`.
fn the_schema_a_reference_names<'contract>(
    contract: &'contract Value,
    schema: &'contract Value,
) -> &'contract Value {
    match schema["$ref"]
        .as_str()
        .and_then(|reference| reference.strip_prefix("#/components/schemas/"))
    {
        Some(schema_name) => &contract["components"]["schemas"][schema_name],
        None => schema,
    }
}

/// The values a keyword lists, a lone value read as a list of one, each as its JSON text.
fn the_values_listed_under(keyword: Option<&Value>) -> BTreeSet<String> {
    match keyword {
        Some(Value::Array(values)) => values.iter().map(Value::to_string).collect(),
        Some(value) => BTreeSet::from([value.to_string()]),
        None => BTreeSet::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn a_contract_at_version_one() -> Value {
        json!({
            "info": { "x-contract-version": 1 },
            "methods": [
                {
                    "name": "list_streams",
                    "params": [{ "name": "params", "schema": { "$ref": "#/components/schemas/ListStreamsRequest" } }],
                    "result": { "name": "list_streams_result", "schema": { "$ref": "#/components/schemas/ListStreamsResult" } },
                    "x-since": 1,
                    "x-stability": "stable",
                },
                {
                    "name": "write_graph_input",
                    "params": [{ "name": "params", "schema": { "$ref": "#/components/schemas/WriteGraphInputRequest" } }],
                    "result": { "name": "write_graph_input_result", "schema": true },
                    "x-since": 1,
                    "x-stability": "experimental",
                },
            ],
            "x-notifications": [],
            "x-agent-discovery-tools": [{ "name": "call_operation", "params": { "type": "object" } }],
            "components": { "schemas": {
                "ListStreamsRequest": {
                    "type": "object",
                    "properties": { "state": { "enum": ["running", "stopped"] } },
                },
                "ListStreamsResult": {
                    "type": "object",
                    "properties": { "streams": { "type": "array", "items": { "$ref": "#/components/schemas/ListedStream" } } },
                    "required": ["streams"],
                },
                "ListedStream": {
                    "type": "object",
                    "properties": {
                        "stream": { "type": "string" },
                        "display_name": { "type": ["string", "null"] },
                    },
                    "required": ["stream"],
                },
                "WriteGraphInputRequest": {
                    "type": "object",
                    "properties": { "value": { "type": "number" } },
                },
            } },
        })
    }

    #[test]
    fn a_removed_result_field_without_a_version_bump_is_refused_naming_the_field() {
        let earlier = a_contract_at_version_one();
        let mut later = earlier.clone();
        later["components"]["schemas"]["ListedStream"]["properties"]
            .as_object_mut()
            .unwrap()
            .remove("display_name");

        let refusal = the_refusal_of_a_contract_change(&earlier, &later).unwrap();

        assert!(
            refusal.contains("lost the field \"display_name\""),
            "{refusal}"
        );
        assert!(
            refusal.contains("`list_streams` result.streams.items"),
            "{refusal}"
        );
    }

    #[test]
    fn a_removed_field_with_a_version_bump_passes() {
        let earlier = a_contract_at_version_one();
        let mut later = earlier.clone();
        later["components"]["schemas"]["ListedStream"]["properties"]
            .as_object_mut()
            .unwrap()
            .remove("display_name");
        later["info"]["x-contract-version"] = json!(2);

        assert_eq!(the_refusal_of_a_contract_change(&earlier, &later), None);
    }

    #[test]
    fn an_added_operation_and_an_added_optional_field_pass_with_no_version_bump() {
        let earlier = a_contract_at_version_one();
        let mut later = earlier.clone();
        later["methods"].as_array_mut().unwrap().push(json!({
            "name": "describe_runtime",
            "params": [{ "name": "params", "schema": { "type": "object" } }],
            "result": { "name": "describe_runtime_result", "schema": { "type": "object" } },
            "x-since": 1,
            "x-stability": "stable",
        }));
        later["components"]["schemas"]["ListedStream"]["properties"]["node_count"] =
            json!({ "type": "integer" });
        later["components"]["schemas"]["ListStreamsRequest"]["properties"]["project"] =
            json!({ "type": "string" });

        assert_eq!(the_refusal_of_a_contract_change(&earlier, &later), None);
    }

    #[test]
    fn an_added_operation_dated_before_the_change_is_refused() {
        let earlier = a_contract_at_version_one();
        let mut later = earlier.clone();
        later["info"]["x-contract-version"] = json!(3);
        later["methods"].as_array_mut().unwrap().push(json!({
            "name": "describe_runtime",
            "x-since": 0,
            "x-stability": "stable",
        }));

        let refusal = the_refusal_of_a_contract_change(&earlier, &later).unwrap();

        assert!(refusal.contains("\"describe_runtime\""), "{refusal}");
    }

    #[test]
    fn each_kind_of_break_is_named() {
        let earlier = a_contract_at_version_one();
        for (break_the_contract, what_the_refusal_names) in [
            (
                (|later: &mut Value| {
                    later["methods"].as_array_mut().unwrap().remove(0);
                }) as fn(&mut Value),
                "`list_streams` was removed from `methods`",
            ),
            (
                |later: &mut Value| {
                    later["x-agent-discovery-tools"] = json!([]);
                },
                "`call_operation` was removed from `x-agent-discovery-tools`",
            ),
            (
                |later: &mut Value| {
                    later["components"]["schemas"]["ListStreamsRequest"]["required"] =
                        json!(["state"]);
                },
                "`list_streams` params now requires the field \"state\"",
            ),
            (
                |later: &mut Value| {
                    later["components"]["schemas"]["ListStreamsRequest"]["properties"]["state"]["enum"] =
                        json!(["running"]);
                },
                "`list_streams` params.state no longer takes the value \"stopped\"",
            ),
            (
                |later: &mut Value| {
                    later["components"]["schemas"]["ListedStream"]["required"] = json!([]);
                },
                "no longer always carries the field \"stream\"",
            ),
            (
                |later: &mut Value| {
                    later["components"]["schemas"]["ListedStream"]["properties"]["stream"]["type"] =
                        json!(["string", "null"]);
                },
                "result.streams.items.stream may now be the type \"null\"",
            ),
        ] {
            let mut later = earlier.clone();
            break_the_contract(&mut later);

            let refusal = the_refusal_of_a_contract_change(&earlier, &later)
                .unwrap_or_else(|| panic!("no refusal names {what_the_refusal_names}"));

            assert!(refusal.contains(what_the_refusal_names), "{refusal}");
        }
    }

    #[test]
    fn an_experimental_operation_changes_with_no_version_bump() {
        let earlier = a_contract_at_version_one();
        let mut later = earlier.clone();
        later["components"]["schemas"]["WriteGraphInputRequest"]["properties"] = json!({});
        later["methods"].as_array_mut().unwrap().remove(1);

        assert_eq!(the_refusal_of_a_contract_change(&earlier, &later), None);
    }

    #[test]
    fn a_contract_version_that_goes_down_is_refused() {
        let later = a_contract_at_version_one();
        let mut earlier = later.clone();
        earlier["info"]["x-contract-version"] = json!(2);

        assert!(the_refusal_of_a_contract_change(&earlier, &later).is_some());
    }

    #[test]
    fn the_committed_contract_file_breaks_no_client_of_itself() {
        let committed: Value = serde_json::from_str(include_str!(
            "../../runtime/streamlib-control-contract/contract.json"
        ))
        .unwrap();

        assert_eq!(
            the_breaking_differences_between(&committed, &committed),
            Vec::<String>::new()
        );
    }
}
