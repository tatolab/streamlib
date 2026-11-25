// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;
use streamlib_runtime_client_contract::local_api_wire_contract::ControlOperationRefusalOnTheWire;

use crate::agent_surface::{
    CALL_OPERATION_TOOL_NAME, DESCRIBE_OPERATIONS_TOOL_NAME, DescribeOperationsRequest,
    LIST_OPERATION_GROUPS_TOOL_NAME, ListOperationGroupsRequest,
};
use crate::control_operation::{
    ControlOperationDeclaration, ControlOperationExample, every_field_is_written_back,
};
use crate::control_operations::{CONTROL_OPERATIONS, ControlOperationCall};

/// Each registered operation with its example.
fn every_example() -> impl Iterator<
    Item = (
        &'static ControlOperationDeclaration,
        ControlOperationExample,
    ),
> {
    CONTROL_OPERATIONS
        .iter()
        .filter_map(|declaration| declaration.example.map(|example| (declaration, example)))
}

/// Every value an example renders: its params, its answer and its refusal.
fn every_value_of(example: &ControlOperationExample) -> Vec<Value> {
    let mut values = vec![(example.params)()];
    values.extend(example.answer.map(|answer| answer()));
    values.extend(example.refusal.map(|refusal| refusal()));
    values
}

/// Every object under `value` holding `key`, `value` itself included.
fn every_object_holding<'value>(
    value: &'value Value,
    key: &str,
    found: &mut Vec<&'value serde_json::Map<String, Value>>,
) {
    match value {
        Value::Object(object) => {
            if object.contains_key(key) {
                found.push(object);
            }
            for inner in object.values() {
                every_object_holding(inner, key, found);
            }
        }
        Value::Array(items) => {
            for item in items {
                every_object_holding(item, key, found);
            }
        }
        _ => {}
    }
}

/// Whether `params` read as the request of the operation or discovery tool named `operation`.
fn the_params_read_as_the_request_of(operation: &str, params: &Value) -> Result<(), String> {
    match operation {
        DESCRIBE_OPERATIONS_TOOL_NAME => {
            serde_json::from_value::<DescribeOperationsRequest>(params.clone())
                .map(|_| ())
                .map_err(|error| error.to_string())
        }
        LIST_OPERATION_GROUPS_TOOL_NAME => {
            serde_json::from_value::<ListOperationGroupsRequest>(params.clone())
                .map(|_| ())
                .map_err(|error| error.to_string())
        }
        _ => ControlOperationCall::parse(operation, Some(params.clone()))
            .map(|_| ())
            .map_err(|refusal| refusal.to_string()),
    }
}

#[test]
fn every_operation_the_discovery_tools_list_has_an_example_and_a_replaced_one_has_none() {
    for declaration in CONTROL_OPERATIONS {
        assert_eq!(
            declaration.example.is_some(),
            declaration.is_listed_by_the_discovery_tools(),
            "{}",
            declaration.method_name
        );
    }
}

#[test]
fn every_example_sends_params_its_operation_parses() {
    for (declaration, example) in every_example() {
        let params = (example.params)();
        assert!(params.is_object(), "{}", declaration.method_name);
        if let Err(refused) = the_params_read_as_the_request_of(declaration.method_name, &params) {
            panic!("{}'s example params: {refused}", declaration.method_name);
        }
    }
}

#[test]
fn every_example_answer_reads_back_as_its_operations_result() {
    for (declaration, example) in every_example() {
        let Some(answer) = example.answer else {
            continue;
        };
        if let Err(refused) = (declaration.reads_back_as_its_result)(&answer()) {
            panic!("{}'s example answer: {refused}", declaration.method_name);
        }
    }
}

#[test]
fn every_example_refusal_is_the_one_refusal_shape_with_its_code_and_its_operation() {
    for (declaration, example) in every_example() {
        let Some(refusal) = example.refusal else {
            continue;
        };
        let refusal = refusal();
        assert!(
            refusal.get("refusal_code").is_some(),
            "{}'s example refusal states no code",
            declaration.method_name
        );
        let read: ControlOperationRefusalOnTheWire = serde_json::from_value(refusal.clone())
            .unwrap_or_else(|error| {
                panic!("{}'s example refusal: {error}", declaration.method_name)
            });
        assert_eq!(read.action, declaration.method_name);
        assert!(
            read.runtime_words
                .starts_with(&format!("{} failed: ", declaration.method_name)),
            "{}",
            read.runtime_words
        );
        let written = serde_json::to_value(&read).unwrap_or_default();
        assert!(
            every_field_is_written_back(&refusal, &written),
            "{}'s example refusal reads back as {written}",
            declaration.method_name
        );
    }
}

#[test]
fn every_suggested_call_in_an_example_parses_as_the_suggested_operations_request() {
    for (declaration, example) in every_example() {
        for value in every_value_of(&example) {
            let mut suggested = Vec::new();
            every_object_holding(&value, "suggested_call", &mut suggested);
            for holder in suggested {
                let call = &holder["suggested_call"];
                let operation = call["operation"].as_str().unwrap_or_default();
                if let Err(refused) = the_params_read_as_the_request_of(operation, &call["params"])
                {
                    panic!(
                        "{}'s example suggests `{operation}` with params it refuses: {refused}",
                        declaration.method_name
                    );
                }
            }
        }
    }
}

#[test]
fn no_example_calls_or_suggests_a_replaced_operation() {
    let replaced: BTreeSet<&str> = CONTROL_OPERATIONS
        .iter()
        .filter(|declaration| declaration.replaced_by.is_some())
        .map(|declaration| declaration.method_name)
        .collect();
    for (declaration, example) in every_example() {
        for value in every_value_of(&example) {
            let mut named = Vec::new();
            every_object_holding(&value, "operation", &mut named);
            for holder in named {
                let operation = holder["operation"].as_str().unwrap_or_default();
                assert!(
                    !replaced.contains(operation),
                    "{}'s example names the replaced `{operation}`",
                    declaration.method_name
                );
            }
        }
    }
}

/// Every operation the three scripted agent sessions call, as a tool of its own or through
/// `call_operation`.
fn the_operations_the_agent_loops_call() -> BTreeSet<String> {
    let scripts = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/stream-on-runtime/agent_loop_scripts");
    let mut operations = BTreeSet::new();
    for script in [
        "build_a_graph",
        "author_a_python_node",
        "author_a_node_ui_element",
    ] {
        let path = scripts.join(format!("{script}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let script: Value = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        for step in script["steps"].as_array().into_iter().flatten() {
            let tool = step["tool"].as_str().unwrap_or_default();
            let operation = if tool == CALL_OPERATION_TOOL_NAME {
                step["arguments"]["operation"].as_str().unwrap_or_default()
            } else {
                tool
            };
            operations.insert(operation.to_owned());
        }
    }
    operations
}

#[test]
fn every_operation_the_three_agent_loops_call_shows_an_answer_and_a_refusal() {
    let called = the_operations_the_agent_loops_call();
    assert!(called.contains("check_link"), "{called:?}");
    assert!(called.contains("create_node_ui_element"), "{called:?}");
    for operation in &called {
        let declaration = CONTROL_OPERATIONS
            .iter()
            .find(|declaration| declaration.method_name == operation)
            .unwrap_or_else(|| panic!("the loops call `{operation}`, which is no operation"));
        let example = declaration
            .example
            .unwrap_or_else(|| panic!("`{operation}` has no example"));
        assert!(example.answer.is_some(), "`{operation}` shows no answer");
        assert!(example.refusal.is_some(), "`{operation}` shows no refusal");
    }
}
