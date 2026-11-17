// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::*;

fn params(params: Value) -> Map<String, Value> {
    params.as_object().cloned().unwrap()
}

#[test]
fn a_call_no_agent_can_make_is_never_suggested() {
    assert_eq!(the_suggested_call("expose_port", Map::new()), None);
    assert_eq!(
        the_suggested_call("list_streams", params(json!({"stream": "x"}))),
        None,
        "params its request does not read"
    );
    assert_eq!(
        the_suggested_call(
            DESCRIBE_OPERATIONS_TOOL_NAME,
            params(json!({"operations": "graph"}))
        ),
        None
    );
    assert_eq!(
        the_suggested_call("list_streams", Map::new()),
        Some(SuggestedOperationCall {
            operation: "list_streams".to_owned(),
            params: Map::new(),
        })
    );
}

#[test]
fn params_that_do_not_match_name_the_value_by_json_pointer() {
    let refusal = ControlOperationCall::parse(
        "transact",
        Some(json!({"operations": [], "mode": "sideways"})),
    )
    .unwrap_err();

    let on_the_wire = refusal.the_refusal_on_the_wire();

    assert_eq!(
        on_the_wire.refusal_code,
        ControlOperationRefusalCode::ParamsDoNotMatchSchema
    );
    assert_eq!(on_the_wire.action, "transact");
    assert_eq!(on_the_wire.refused_params_pointer.as_deref(), Some("/mode"));
    assert_eq!(on_the_wire.runtime_words, refusal.to_string());
    assert_eq!(
        on_the_wire.suggested_call,
        Some(SuggestedOperationCall {
            operation: DESCRIBE_OPERATIONS_TOOL_NAME.to_owned(),
            params: params(json!({"operations": ["transact"]})),
        })
    );
}

#[test]
fn a_pointer_escapes_its_tokens_and_is_absent_for_the_params_as_a_whole() {
    #[derive(Debug, serde::Deserialize)]
    #[expect(dead_code, reason = "only its read is under test")]
    struct Nested {
        values: std::collections::BTreeMap<String, Vec<u32>>,
    }

    let refused =
        the_params_read_as::<Nested>(json!({"values": {"a/b~c": [1, "two"]}})).unwrap_err();
    assert_eq!(refused.pointer.as_deref(), Some("/values/a~1b~0c/1"));

    let refused = the_params_read_as::<Nested>(json!({})).unwrap_err();
    assert_eq!(refused.pointer, None);
    assert_eq!(refused.why, "missing field `values`");
}

#[test]
fn a_method_no_operation_has_is_refused_suggesting_the_listing() {
    let on_the_wire = ControlOperationCall::parse("expose_port", None)
        .unwrap_err()
        .the_refusal_on_the_wire();

    assert_eq!(
        on_the_wire.refusal_code,
        ControlOperationRefusalCode::OperationNotFound
    );
    assert_eq!(on_the_wire.action, "expose_port");
    assert_eq!(
        on_the_wire.suggested_call.map(|call| call.operation),
        Some(LIST_OPERATION_GROUPS_TOOL_NAME.to_owned())
    );
}
