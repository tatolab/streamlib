// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::{Value, json};

use super::*;

fn a_contract_at_version_one() -> Value {
    json!({
        "info": { "x-contract-version": 1, "x-contract-revision": 0 },
        "methods": [
            {
                "name": "list_streams",
                "params": [{ "name": "params", "schema": { "$ref": "#/components/schemas/ListStreamsRequest" } }],
                "result": { "name": "list_streams_result", "schema": { "$ref": "#/components/schemas/ListStreamsResult" } },
                "errors": [
                    { "code": 1, "message": "refused", "data": { "$ref": "#/components/schemas/Refusal" } },
                    { "code": 3, "message": "contract mismatch", "data": { "type": "object" } },
                ],
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
        "x-pin-types": [
            {
                "name": "color",
                "flows": ["value", "stream"],
                "annotations": ["meaning", "control"],
                "value_json_schema": { "$ref": "#/components/schemas/ColorPinValue" },
                "x-since": 1,
                "x-stability": "stable",
            },
            {
                "name": "vector",
                "flows": ["value"],
                "annotations": [],
                "value_json_schema": { "$ref": "#/components/schemas/VectorPinValue" },
                "x-since": 1,
                "x-stability": "experimental",
            },
        ],
        "components": { "schemas": {
            "ListStreamsRequest": {
                "type": "object",
                "properties": {
                    "state": { "enum": ["running", "stopped"] },
                    "filter": { "$ref": "#/components/schemas/StreamFilter" },
                },
            },
            "StreamFilter": { "oneOf": [
                {
                    "type": "object",
                    "properties": { "by": { "type": "string", "const": "name" }, "name": { "type": "string" } },
                    "required": ["by", "name"],
                },
                {
                    "type": "object",
                    "properties": { "by": { "type": "string", "const": "project" }, "project": { "type": "string" } },
                    "required": ["by", "project"],
                },
            ] },
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
                    "state": { "$ref": "#/components/schemas/ListedStreamState" },
                    "origin": { "$ref": "#/components/schemas/StreamOrigin" },
                    "frame_rate": { "anyOf": [{ "type": "number" }, { "type": "null" }] },
                },
                "required": ["stream"],
            },
            "ListedStreamState": { "oneOf": [
                { "type": "string", "const": "running" },
                { "type": "string", "const": "stopped" },
            ] },
            "StreamOrigin": { "oneOf": [
                {
                    "type": "object",
                    "properties": { "source": { "type": "string", "const": "project" }, "project": { "type": "string" } },
                    "required": ["source", "project"],
                },
                {
                    "type": "object",
                    "properties": { "source": { "type": "string", "const": "dependency" } },
                    "required": ["source"],
                },
            ] },
            "Refusal": {
                "type": "object",
                "properties": { "refusal_code": { "$ref": "#/components/schemas/RefusalCode", "default": "refused" } },
            },
            "RefusalCode": {
                "oneOf": [
                    { "type": "string", "const": "stream_not_found" },
                    { "type": "string", "const": "refused" },
                ],
                "x-may-grow": true,
            },
            "WriteGraphInputRequest": {
                "type": "object",
                "properties": { "value": { "type": "number" } },
            },
            "ColorPinValue": {
                "type": "object",
                "properties": { "red": { "type": "number" }, "alpha": { "type": "number" } },
                "required": ["red"],
            },
            "VectorPinValue": {
                "type": "object",
                "properties": { "x": { "type": "number" } },
                "required": ["x"],
            },
        } },
    })
}

/// `earlier` changed by `change`, with its revision raised, so only what the change breaks is
/// judged.
fn an_addition_to(earlier: &Value, change: impl FnOnce(&mut Value)) -> Value {
    let mut later = earlier.clone();
    change(&mut later);
    later["info"]["x-contract-revision"] =
        json!(the_counter_of(earlier, "x-contract-revision") + 1);
    later
}

fn the_schemas_of(contract: &mut Value) -> &mut Value {
    &mut contract["components"]["schemas"]
}

fn the_refusal_names(earlier: &Value, later: &Value, what_the_refusal_names: &str) {
    let refusal = the_refusal_of_a_contract_change(earlier, later)
        .unwrap_or_else(|| panic!("no refusal names {what_the_refusal_names}"));
    assert!(refusal.contains(what_the_refusal_names), "{refusal}");
}

#[test]
fn a_removed_result_field_without_a_version_bump_is_refused_naming_the_field() {
    let earlier = a_contract_at_version_one();
    let later = an_addition_to(&earlier, |later| {
        the_schemas_of(later)["ListedStream"]["properties"]
            .as_object_mut()
            .unwrap()
            .remove("display_name");
    });

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
    the_schemas_of(&mut later)["ListedStream"]["properties"]
        .as_object_mut()
        .unwrap()
        .remove("display_name");
    later["info"]["x-contract-version"] = json!(2);

    assert_eq!(the_refusal_of_a_contract_change(&earlier, &later), None);
}

#[test]
fn an_added_operation_and_an_added_optional_field_pass_with_the_revision_raised() {
    let earlier = a_contract_at_version_one();
    let later = an_addition_to(&earlier, |later| {
        later["methods"].as_array_mut().unwrap().push(json!({
            "name": "describe_runtime",
            "params": [{ "name": "params", "schema": { "type": "object" } }],
            "result": { "name": "describe_runtime_result", "schema": { "type": "object" } },
            "x-since": 1,
            "x-stability": "stable",
        }));
        the_schemas_of(later)["ListedStream"]["properties"]["node_count"] =
            json!({ "type": "integer" });
        the_schemas_of(later)["ListStreamsRequest"]["properties"]["project"] =
            json!({ "type": "string" });
    });

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

    the_refusal_names(&earlier, &later, "\"describe_runtime\"");
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
                the_schemas_of(later)["ListStreamsRequest"]["required"] = json!(["state"]);
            },
            "`list_streams` params now requires the field \"state\"",
        ),
        (
            |later: &mut Value| {
                the_schemas_of(later)["ListStreamsRequest"]["properties"]["state"]["enum"] =
                    json!(["running"]);
            },
            "`list_streams` params.state no longer takes the value \"stopped\"",
        ),
        (
            |later: &mut Value| {
                the_schemas_of(later)["ListedStream"]["required"] = json!([]);
            },
            "no longer always carries the field \"stream\"",
        ),
        (
            |later: &mut Value| {
                the_schemas_of(later)["ListedStream"]["properties"]["stream"]["type"] =
                    json!(["string", "null"]);
            },
            "result.streams.items.stream may now be the type \"null\"",
        ),
        (
            |later: &mut Value| {
                later["methods"][0]["errors"]
                    .as_array_mut()
                    .unwrap()
                    .remove(1);
            },
            "`list_streams` no longer answers the error code 3",
        ),
    ] {
        let later = an_addition_to(&earlier, break_the_contract);

        the_refusal_names(&earlier, &later, what_the_refusal_names);
    }
}

#[test]
fn a_refusal_code_removed_from_an_answer_is_refused_though_its_set_may_grow() {
    let earlier = a_contract_at_version_one();
    let later = an_addition_to(&earlier, |later| {
        the_schemas_of(later)["RefusalCode"]["oneOf"]
            .as_array_mut()
            .unwrap()
            .remove(0);
    });

    the_refusal_names(
        &earlier,
        &later,
        "`list_streams` error 1 data.refusal_code.oneOf can no longer be the value \"stream_not_found\"",
    );
}

#[test]
fn a_value_removed_from_a_closed_set_in_an_answer_is_refused() {
    let earlier = a_contract_at_version_one();
    let later = an_addition_to(&earlier, |later| {
        the_schemas_of(later)["ListedStreamState"]["oneOf"]
            .as_array_mut()
            .unwrap()
            .remove(1);
    });

    the_refusal_names(
        &earlier,
        &later,
        "result.streams.items.state.oneOf can no longer be the value \"stopped\"",
    );
}

#[test]
fn a_value_added_to_a_set_marked_to_grow_passes() {
    let earlier = a_contract_at_version_one();
    let later = an_addition_to(&earlier, |later| {
        the_schemas_of(later)["RefusalCode"]["oneOf"]
            .as_array_mut()
            .unwrap()
            .insert(0, json!({ "type": "string", "const": "node_not_found" }));
    });

    assert_eq!(the_refusal_of_a_contract_change(&earlier, &later), None);
}

#[test]
fn a_value_added_to_a_closed_set_in_an_answer_is_refused() {
    let earlier = a_contract_at_version_one();
    let later = an_addition_to(&earlier, |later| {
        the_schemas_of(later)["ListedStreamState"]["oneOf"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "type": "string", "const": "paused" }));
    });

    the_refusal_names(
        &earlier,
        &later,
        "result.streams.items.state.oneOf may now be the value \"paused\", and the set is not \
         marked `x-may-grow`",
    );
}

#[test]
fn a_keyword_beside_a_ref_is_compared_at_each_use_of_the_component() {
    let with_a_first_stream_narrowed = |state_values: &[&str]| {
        let mut contract = a_contract_at_version_one();
        let states: Vec<Value> = state_values
            .iter()
            .map(|state| json!({ "type": "string", "const": state }))
            .collect();
        the_schemas_of(&mut contract)["ListStreamsResult"]["properties"]["first"] = json!({
            "$ref": "#/components/schemas/ListedStream",
            "properties": { "state": { "oneOf": states } },
        });
        contract
    };
    let earlier = with_a_first_stream_narrowed(&["running"]);
    let mut later = with_a_first_stream_narrowed(&["running", "paused"]);
    later["info"]["x-contract-revision"] =
        json!(the_counter_of(&earlier, "x-contract-revision") + 1);

    the_refusal_names(
        &earlier,
        &later,
        "result.first.state.oneOf may now be the value \"paused\", and the set is not marked \
         `x-may-grow`",
    );
}

#[test]
fn an_alternative_added_to_a_closed_union_in_an_answer_is_refused_by_its_tag_alone() {
    let earlier = a_contract_at_version_one();
    let later = an_addition_to(&earlier, |later| {
        the_schemas_of(later)["StreamOrigin"]["oneOf"]
            .as_array_mut()
            .unwrap()
            .insert(
                0,
                json!({
                    "type": "object",
                    "properties": { "source": { "type": "string", "const": "built_in" } },
                    "required": ["source"],
                }),
            );
    });

    let refusal = the_refusal_of_a_contract_change(&earlier, &later).unwrap();

    assert!(
        refusal
            .contains("origin.oneOf may now be the alternative whose \"source\" is \"built_in\""),
        "{refusal}"
    );
    assert_eq!(
        refusal
            .lines()
            .filter(|line| line.starts_with("- "))
            .count(),
        1,
        "{refusal}"
    );
}

#[test]
fn an_untagged_alternative_inserted_first_in_an_answer_is_refused_naming_it_alone() {
    let earlier = a_contract_at_version_one();
    let later = an_addition_to(&earlier, |later| {
        the_schemas_of(later)["ListedStream"]["properties"]["frame_rate"]["anyOf"]
            .as_array_mut()
            .unwrap()
            .insert(0, json!({ "type": "string" }));
    });

    let refusal = the_refusal_of_a_contract_change(&earlier, &later).unwrap();

    assert!(
        refusal.contains("frame_rate.anyOf may now be the alternative of type \"string\""),
        "{refusal}"
    );
    assert_eq!(
        refusal
            .lines()
            .filter(|line| line.starts_with("- "))
            .count(),
        1,
        "{refusal}"
    );
}

#[test]
fn an_untagged_alternative_removed_first_from_an_answer_passes() {
    let earlier = a_contract_at_version_one();
    let later = an_addition_to(&earlier, |later| {
        the_schemas_of(later)["ListedStream"]["properties"]["frame_rate"]["anyOf"]
            .as_array_mut()
            .unwrap()
            .remove(0);
    });

    assert_eq!(the_refusal_of_a_contract_change(&earlier, &later), None);
}

#[test]
fn an_untagged_alternative_inserted_first_in_what_a_client_sends_passes() {
    let earlier = an_addition_to(&a_contract_at_version_one(), |earlier| {
        the_schemas_of(earlier)["ListStreamsRequest"]["properties"]["limit"] =
            json!({ "anyOf": [{ "type": "string" }] });
    });
    let later = an_addition_to(&earlier, |later| {
        the_schemas_of(later)["ListStreamsRequest"]["properties"]["limit"]["anyOf"]
            .as_array_mut()
            .unwrap()
            .insert(0, json!({ "type": "integer" }));
    });

    assert_eq!(the_refusal_of_a_contract_change(&earlier, &later), None);
}

#[test]
fn an_untagged_alternative_removed_first_from_what_a_client_sends_is_refused() {
    let earlier = an_addition_to(&a_contract_at_version_one(), |earlier| {
        the_schemas_of(earlier)["ListStreamsRequest"]["properties"]["limit"] =
            json!({ "anyOf": [{ "type": "integer" }, { "type": "string" }] });
    });
    let later = an_addition_to(&earlier, |later| {
        the_schemas_of(later)["ListStreamsRequest"]["properties"]["limit"]["anyOf"]
            .as_array_mut()
            .unwrap()
            .remove(0);
    });

    let refusal = the_refusal_of_a_contract_change(&earlier, &later).unwrap();

    assert!(
        refusal.contains("params.limit.anyOf no longer takes the alternative of type \"integer\""),
        "{refusal}"
    );
    assert_eq!(
        refusal
            .lines()
            .filter(|line| line.starts_with("- "))
            .count(),
        1,
        "{refusal}"
    );
}

#[test]
fn an_untagged_alternative_whose_type_only_it_states_is_compared_with_its_counterpart() {
    let rate = json!({
        "type": "object",
        "properties": { "numerator": { "type": "integer" }, "denominator": { "type": "integer" } },
        "required": ["numerator", "denominator"],
    });
    let earlier = an_addition_to(&a_contract_at_version_one(), |earlier| {
        the_schemas_of(earlier)["ListedStream"]["properties"]["frame_rate"]["anyOf"] =
            json!([rate, { "type": "null" }]);
    });
    let later = an_addition_to(&earlier, |later| {
        let rates = the_schemas_of(later)["ListedStream"]["properties"]["frame_rate"]["anyOf"]
            .as_array_mut()
            .unwrap();
        rates[0]["required"] = json!(["numerator"]);
        rates.insert(0, json!({ "type": "string" }));
    });

    let refusal = the_refusal_of_a_contract_change(&earlier, &later).unwrap();

    assert!(
        refusal.contains("frame_rate.anyOf[1] no longer always carries the field \"denominator\""),
        "{refusal}"
    );
    assert!(
        refusal.contains("frame_rate.anyOf may now be the alternative of type \"string\""),
        "{refusal}"
    );
    assert_eq!(
        refusal
            .lines()
            .filter(|line| line.starts_with("- "))
            .count(),
        2,
        "{refusal}"
    );
}

#[test]
fn an_alternative_inserted_in_a_union_a_client_sends_is_matched_by_tag_and_passes() {
    let earlier = a_contract_at_version_one();
    let later = an_addition_to(&earlier, |later| {
        the_schemas_of(later)["StreamFilter"]["oneOf"]
            .as_array_mut()
            .unwrap()
            .insert(
                0,
                json!({
                    "type": "object",
                    "properties": { "by": { "type": "string", "const": "state" } },
                    "required": ["by"],
                }),
            );
    });

    assert_eq!(the_refusal_of_a_contract_change(&earlier, &later), None);
}

#[test]
fn a_tagged_alternative_is_compared_with_the_one_of_its_tag_wherever_it_moved() {
    let earlier = a_contract_at_version_one();
    let later = an_addition_to(&earlier, |later| {
        let origins = the_schemas_of(later)["StreamOrigin"]["oneOf"]
            .as_array_mut()
            .unwrap();
        origins.reverse();
        origins[1]["required"] = json!(["source"]);
    });

    the_refusal_names(
        &earlier,
        &later,
        "origin.oneOf[the alternative whose \"source\" is \"project\"] no longer always carries \
         the field \"project\"",
    );
}

#[test]
fn a_tagged_alternative_removed_from_a_union_a_client_sends_is_refused() {
    let earlier = a_contract_at_version_one();
    let later = an_addition_to(&earlier, |later| {
        the_schemas_of(later)["StreamFilter"]["oneOf"]
            .as_array_mut()
            .unwrap()
            .remove(0);
    });

    the_refusal_names(
        &earlier,
        &later,
        "params.filter.oneOf no longer takes the alternative whose \"by\" is \"name\"",
    );
}

#[test]
fn a_stable_operation_marked_experimental_is_refused() {
    let earlier = a_contract_at_version_one();
    let later = an_addition_to(&earlier, |later| {
        later["methods"][0]["x-stability"] = json!("experimental");
    });

    the_refusal_names(
        &earlier,
        &later,
        "`list_streams` in `methods` was stable and is now `experimental`",
    );
}

#[test]
fn a_replacement_named_on_a_stable_operation_passes() {
    let earlier = a_contract_at_version_one();
    let later = an_addition_to(&earlier, |later| {
        later["methods"][0]["x-replaced-by"] = json!("list_project_streams");
    });

    assert_eq!(the_refusal_of_a_contract_change(&earlier, &later), None);
}

#[test]
fn an_experimental_operation_changes_with_the_revision_raised() {
    let earlier = a_contract_at_version_one();
    let later = an_addition_to(&earlier, |later| {
        the_schemas_of(later)["WriteGraphInputRequest"]["properties"] = json!({});
        later["methods"].as_array_mut().unwrap().remove(1);
    });

    assert_eq!(the_refusal_of_a_contract_change(&earlier, &later), None);
}

#[test]
fn an_experimental_entry_moved_to_stable_passes_uncompared() {
    let earlier = a_contract_at_version_one();
    let later = an_addition_to(&earlier, |later| {
        later["x-pin-types"][1]["x-stability"] = json!("stable");
        later["x-pin-types"][1]["flows"] = json!([]);
        the_schemas_of(later)["VectorPinValue"]["required"] = json!(["x", "y"]);
    });

    assert_eq!(the_refusal_of_a_contract_change(&earlier, &later), None);
}

#[test]
fn each_break_of_a_stable_pin_type_row_is_named() {
    let earlier = a_contract_at_version_one();
    for (break_the_contract, what_the_refusal_names) in [
        (
            (|later: &mut Value| {
                later["x-pin-types"].as_array_mut().unwrap().remove(0);
            }) as fn(&mut Value),
            "`color` was removed from `x-pin-types`",
        ),
        (
            |later: &mut Value| {
                later["x-pin-types"][0]["x-stability"] = json!("experimental");
            },
            "`color` in `x-pin-types` was stable and is now `experimental`",
        ),
        (
            |later: &mut Value| {
                later["x-pin-types"][0]["flows"] = json!(["value"]);
            },
            "pin type `color` no longer lists \"stream\" in `flows`",
        ),
        (
            |later: &mut Value| {
                later["x-pin-types"][0]["annotations"] = json!(["control"]);
            },
            "pin type `color` no longer lists \"meaning\" in `annotations`",
        ),
        (
            |later: &mut Value| {
                later["x-pin-types"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("value_json_schema");
            },
            "pin type `color` no longer stores a value",
        ),
        (
            |later: &mut Value| {
                the_schemas_of(later)["ColorPinValue"]["required"] = json!(["red", "alpha"]);
            },
            "pin type `color` value as sent now requires the field \"alpha\"",
        ),
        (
            |later: &mut Value| {
                the_schemas_of(later)["ColorPinValue"]["required"] = json!([]);
            },
            "pin type `color` value as read no longer always carries the field \"red\"",
        ),
    ] {
        let later = an_addition_to(&earlier, break_the_contract);

        the_refusal_names(&earlier, &later, what_the_refusal_names);
    }
}

#[test]
fn a_stable_pin_type_row_that_gains_a_flow_and_an_annotation_passes() {
    let earlier = a_contract_at_version_one();
    let later = an_addition_to(&earlier, |later| {
        later["x-pin-types"][0]["flows"] = json!(["value", "stream", "trigger"]);
        later["x-pin-types"][0]["annotations"] = json!(["meaning", "control", "unit"]);
    });

    assert_eq!(the_refusal_of_a_contract_change(&earlier, &later), None);
}

#[test]
fn a_changed_contract_that_raises_neither_counter_is_refused() {
    let earlier = a_contract_at_version_one();
    let mut later = earlier.clone();
    the_schemas_of(&mut later)["ListedStream"]["properties"]["node_count"] =
        json!({ "type": "integer" });

    the_refusal_names(
        &earlier,
        &later,
        "changed and states contract version 1 revision 0, as the earlier file did at revision 0",
    );
}

#[test]
fn an_unchanged_contract_needs_no_counter_raised() {
    let earlier = a_contract_at_version_one();

    assert_eq!(
        the_refusal_of_a_contract_change(&earlier, &earlier.clone()),
        None
    );
}

#[test]
fn a_raised_version_starts_the_revision_again_at_zero() {
    let earlier = an_addition_to(&a_contract_at_version_one(), |_| {});
    let mut later = earlier.clone();
    later["info"]["x-contract-version"] = json!(2);

    the_refusal_names(
        &earlier,
        &later,
        "starts `CONTROL_CONTRACT_REVISION` again at 0, and the file states revision 1",
    );
}

#[test]
fn a_contract_version_that_goes_down_is_refused() {
    let later = a_contract_at_version_one();
    let mut earlier = later.clone();
    earlier["info"]["x-contract-version"] = json!(2);

    assert!(the_refusal_of_a_contract_change(&earlier, &later).is_some());
}

#[test]
fn a_removal_due_at_the_contract_version_is_refused_and_one_due_later_passes() {
    let earlier = a_contract_at_version_one();
    let marked_for = |version: u64| {
        an_addition_to(&earlier, |later| {
            the_schemas_of(later)["ListedStream"]["properties"]["display_name"]["x-removed-in"] =
                json!(version);
        })
    };

    assert_eq!(
        the_refusal_of_a_contract_change(&earlier, &marked_for(2)),
        None
    );
    the_refusal_names(
        &earlier,
        &marked_for(1),
        "#/components/schemas/ListedStream/properties/display_name states `x-removed-in` 1",
    );
}

#[test]
fn the_committed_contract_file_breaks_no_client_of_itself() {
    let committed: Value = serde_json::from_str(include_str!(
        "../../../runtime/streamlib-control-contract/contract.json"
    ))
    .unwrap();

    assert_eq!(
        the_breaking_differences_between(&committed, &committed),
        Vec::<String>::new()
    );
    assert_eq!(
        the_refusal_of_a_contract_change(&committed, &committed),
        None
    );
}

#[test]
fn a_set_marked_to_grow_in_the_same_change_that_grows_it_is_refused() {
    let earlier = a_contract_at_version_one();
    let later = an_addition_to(&earlier, |later| {
        let state = &mut the_schemas_of(later)["ListedStreamState"];
        state["x-may-grow"] = json!(true);
        state["oneOf"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "type": "string", "const": "sleeping" }));
    });

    the_refusal_names(
        &earlier,
        &later,
        "state.oneOf may now be the value \"sleeping\", and the set is not marked `x-may-grow`",
    );
}
