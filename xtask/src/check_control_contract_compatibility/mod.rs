// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `cargo xtask check-control-contract-compatibility`: the CI gate that fails a change to the
//! control contract file which breaks a client built against the earlier file without bumping
//! `x-contract-version`, and a change that raises neither `x-contract-version` nor
//! `x-contract-revision`.
//!
//! A break is a stable operation, notification, discovery tool or pin type that is gone or now
//! `experimental`, a field that is gone, a field a caller must now send, a field a result no
//! longer always carries, a type or set of values that no longer holds what it held, an error
//! code an operation no longer answers, a pin type row whose flows or annotations shrank, or a
//! set in what a client reads that gained a value or an alternative without the earlier file
//! marking it `x-may-grow`. Alternatives are matched by their tag, or by their shape. An
//! addition is no break; it carries the contract version it arrived in as `x-since`. An entry
//! that was `experimental` in the earlier file may change freely, and its move to `stable` is
//! not compared.

mod schema_comparison;

#[cfg(test)]
mod tests;

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use streamlib_control_contract::CONTROL_CONTRACT_FILE_WORKSPACE_PATH;

use schema_comparison::{ContractComparison, SchemaDirection};

/// Fail when the workspace's contract file breaks a client of the contract file at
/// `earlier_contract_file` without a higher contract version, or differs from it with neither
/// counter raised.
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
fn the_refusal_of_a_contract_change(
    earlier_contract: &Value,
    later_contract: &Value,
) -> Option<String> {
    let earlier_version = the_counter_of(earlier_contract, "x-contract-version");
    let later_version = the_counter_of(later_contract, "x-contract-version");
    if later_version < earlier_version {
        return Some(format!(
            "the contract version went down, from {earlier_version} to {later_version}"
        ));
    }
    if let Some(refusal) = the_refusal_of_the_counters(
        earlier_contract,
        later_contract,
        earlier_version,
        later_version,
    ) {
        return Some(refusal);
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
            the_lines_of(&misdated_additions)
        ));
    }
    let overdue_removals = the_removals_due_by(later_contract, later_version);
    if !overdue_removals.is_empty() {
        return Some(format!(
            "{CONTROL_CONTRACT_FILE_WORKSPACE_PATH} is at contract version {later_version} and \
             still holds what was to be removed by then. Remove it:\n{}",
            the_lines_of(&overdue_removals)
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
        the_lines_of(&breaking_differences)
    ))
}

/// Each finding as one `- ` line.
fn the_lines_of(findings: &[String]) -> String {
    findings
        .iter()
        .map(|finding| format!("- {finding}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One of the counters `info` states: `x-contract-version` or `x-contract-revision`.
fn the_counter_of(contract: &Value, counter: &str) -> u64 {
    contract["info"][counter].as_u64().unwrap_or_default()
}

/// Why the two counters do not carry the change: a changed file at the same version must raise
/// the revision, and a raised version starts the revision again at 0.
fn the_refusal_of_the_counters(
    earlier_contract: &Value,
    later_contract: &Value,
    earlier_version: u64,
    later_version: u64,
) -> Option<String> {
    let earlier_revision = the_counter_of(earlier_contract, "x-contract-revision");
    let later_revision = the_counter_of(later_contract, "x-contract-revision");
    if later_version > earlier_version {
        return (later_revision != 0).then(|| {
            format!(
                "the contract version rose to {later_version}, which starts \
                 `CONTROL_CONTRACT_REVISION` again at 0, and the file states revision \
                 {later_revision}"
            )
        });
    }
    if earlier_contract == later_contract || later_revision > earlier_revision {
        return None;
    }
    Some(format!(
        "{CONTROL_CONTRACT_FILE_WORKSPACE_PATH} changed and states contract version \
         {later_version} revision {later_revision}, as the earlier file did at revision \
         {earlier_revision}. Raise `CONTROL_CONTRACT_REVISION` for an addition, or \
         `CONTROL_CONTRACT_VERSION` for a break"
    ))
}

/// A top-level list of named entries, each compared by `name`.
#[derive(Clone, Copy)]
enum EntryList {
    Methods,
    Notifications,
    DiscoveryTools,
    PinTypes,
}

impl EntryList {
    const ALL: [Self; 4] = [
        Self::Methods,
        Self::Notifications,
        Self::DiscoveryTools,
        Self::PinTypes,
    ];

    /// The list's key in the contract file.
    fn key(self) -> &'static str {
        match self {
            Self::Methods => "methods",
            Self::Notifications => "x-notifications",
            Self::DiscoveryTools => "x-agent-discovery-tools",
            Self::PinTypes => "x-pin-types",
        }
    }

    fn the_entries_of(self, contract: &Value) -> &[Value] {
        the_list_under(&contract[self.key()])
    }
}

/// The values of a JSON array; none for anything else.
fn the_list_under(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

fn the_entry_named<'contract>(
    entries: &'contract [Value],
    name: &Value,
) -> Option<&'contract Value> {
    entries.iter().find(|entry| &entry["name"] == name)
}

fn is_experimental(entry: &Value) -> bool {
    entry["x-stability"] == "experimental"
}

fn the_additions_dated_outside(
    earlier_contract: &Value,
    later_contract: &Value,
    contract_versions_of_the_change: std::ops::RangeInclusive<u64>,
) -> Vec<String> {
    let mut misdated = Vec::new();
    for entry_list in EntryList::ALL {
        let earlier_entries = entry_list.the_entries_of(earlier_contract);
        for added in entry_list
            .the_entries_of(later_contract)
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
                misdated.push(format!("`{}` states `x-since` {since}", added["name"]));
            }
        }
    }
    misdated
}

/// Every `x-removed-in` in `contract` of `contract_version` or lower, one finding each, named by
/// where it sits.
fn the_removals_due_by(contract: &Value, contract_version: u64) -> Vec<String> {
    fn walk(value: &Value, place: &str, contract_version: u64, overdue: &mut Vec<String>) {
        match value {
            Value::Object(object) => {
                if let Some(removed_in) = object.get("x-removed-in")
                    && removed_in
                        .as_u64()
                        .is_none_or(|removed_in| removed_in <= contract_version)
                {
                    overdue.push(format!("{place} states `x-removed-in` {removed_in}"));
                }
                for (key, inner) in object {
                    walk(inner, &format!("{place}/{key}"), contract_version, overdue);
                }
            }
            Value::Array(values) => {
                for (index, inner) in values.iter().enumerate() {
                    walk(
                        inner,
                        &format!("{place}/{index}"),
                        contract_version,
                        overdue,
                    );
                }
            }
            _ => {}
        }
    }
    let mut overdue = Vec::new();
    walk(contract, "#", contract_version, &mut overdue);
    overdue
}

/// Every difference from `earlier_contract` to `later_contract` that breaks a client built
/// against the earlier one, one finding each.
fn the_breaking_differences_between(
    earlier_contract: &Value,
    later_contract: &Value,
) -> Vec<String> {
    let mut comparison = ContractComparison::new(earlier_contract, later_contract);
    for entry_list in EntryList::ALL {
        let list = entry_list.key();
        let later_entries = entry_list.the_entries_of(later_contract);
        for earlier_entry in entry_list.the_entries_of(earlier_contract) {
            if is_experimental(earlier_entry) {
                continue;
            }
            let name = earlier_entry["name"].as_str().unwrap_or_default();
            let Some(later_entry) = the_entry_named(later_entries, &earlier_entry["name"]) else {
                comparison.refuse(format!("`{name}` was removed from `{list}`"));
                continue;
            };
            if is_experimental(later_entry) {
                comparison.refuse(format!(
                    "`{name}` in `{list}` was stable and is now `experimental`"
                ));
            }
            match entry_list {
                EntryList::Methods | EntryList::DiscoveryTools => {
                    compare_operation(&mut comparison, name, earlier_entry, later_entry)
                }
                EntryList::Notifications => comparison.compare_schemas(
                    &format!("`{name}` params"),
                    the_params_schema_of(earlier_entry),
                    the_params_schema_of(later_entry),
                    SchemaDirection::ReadByAClient,
                ),
                EntryList::PinTypes => {
                    compare_pin_type_row(&mut comparison, name, earlier_entry, later_entry)
                }
            }
        }
    }
    comparison.into_breaking_differences()
}

/// An operation or a discovery tool: what a client sends, what it reads back, and the errors it
/// may be answered.
fn compare_operation<'contracts>(
    comparison: &mut ContractComparison<'contracts>,
    name: &str,
    earlier_entry: &'contracts Value,
    later_entry: &'contracts Value,
) {
    comparison.compare_schemas(
        &format!("`{name}` params"),
        the_params_schema_of(earlier_entry),
        the_params_schema_of(later_entry),
        SchemaDirection::SentByAClient,
    );
    comparison.compare_schemas(
        &format!("`{name}` result"),
        the_result_schema_of(earlier_entry),
        the_result_schema_of(later_entry),
        SchemaDirection::ReadByAClient,
    );
    let later_errors = the_list_under(&later_entry["errors"]);
    for earlier_error in the_list_under(&earlier_entry["errors"]) {
        let code = &earlier_error["code"];
        match later_errors.iter().find(|error| &error["code"] == code) {
            Some(later_error) => comparison.compare_schemas(
                &format!("`{name}` error {code} data"),
                &earlier_error["data"],
                &later_error["data"],
                SchemaDirection::ReadByAClient,
            ),
            None => comparison.refuse(format!("`{name}` no longer answers the error code {code}")),
        }
    }
}

/// A pin type row: a client declares ports with its flows and annotations, and both sends and
/// reads its value.
fn compare_pin_type_row<'contracts>(
    comparison: &mut ContractComparison<'contracts>,
    name: &str,
    earlier_row: &'contracts Value,
    later_row: &'contracts Value,
) {
    for listed in ["flows", "annotations"] {
        let later_values = the_list_under(&later_row[listed]);
        for removed in the_list_under(&earlier_row[listed])
            .iter()
            .filter(|value| !later_values.contains(value))
        {
            comparison.refuse(format!(
                "pin type `{name}` no longer lists {removed} in `{listed}`"
            ));
        }
    }
    let Some(earlier_value_schema) = earlier_row.get("value_json_schema") else {
        return;
    };
    let Some(later_value_schema) = later_row.get("value_json_schema") else {
        comparison.refuse(format!("pin type `{name}` no longer stores a value"));
        return;
    };
    for direction in [
        SchemaDirection::SentByAClient,
        SchemaDirection::ReadByAClient,
    ] {
        comparison.compare_schemas(
            &format!("pin type `{name}` value {}", direction.as_words()),
            earlier_value_schema,
            later_value_schema,
            direction,
        );
    }
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
