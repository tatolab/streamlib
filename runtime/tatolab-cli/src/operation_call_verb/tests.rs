// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::*;
use crate::stub_local_api_server::{
    RecordedToolCall, StubLocalApiScript, StubLocalApiServer, StubToolAnswer, StubToolCallTransport,
};

fn params(params: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    params.as_object().cloned().unwrap()
}

#[test]
fn a_listed_tool_is_called_by_its_name_and_any_other_operation_through_call_operation() {
    let stub_local_api_server = StubLocalApiServer::serve(StubLocalApiScript {
        listed_tool_names: vec!["graph".to_owned(), "describe_operations".to_owned()],
        ..StubLocalApiScript::answering_every_tool_call_with(StubToolAnswer::tool_result("{}"))
    });

    for (operation, operation_params) in [
        ("graph", json!({"stream": "camera"})),
        ("describe_operations", json!({"operations": ["add_node"]})),
        ("add_node", json!({"type": "effects:Blur"})),
    ] {
        let exit_code = call_an_operation(
            &stub_local_api_server.local_api_socket_path,
            operation,
            params(operation_params),
        )
        .unwrap();
        assert_eq!(exit_code, 0, "{operation}");
    }

    assert_eq!(
        stub_local_api_server.recorded_tool_names_called(),
        ["graph", "describe_operations", "call_operation"]
    );
    assert_eq!(
        stub_local_api_server.recorded_tool_calls()[2],
        RecordedToolCall {
            tool_name: "add_node".to_owned(),
            tool_arguments: json!({"type": "effects:Blur"}),
            tool_call_transport: StubToolCallTransport::StreamableHttpPost,
        }
    );
}

#[test]
fn a_refused_call_exits_one() {
    let refusal = r#"{"refusal_code":"operation_not_found","action":"nope","runtime_words":"no"}"#;
    let stub_local_api_server = StubLocalApiServer::serve_answering_every_tool_call_with(
        StubToolAnswer::tool_failure(refusal),
    );

    let exit_code = call_an_operation(
        &stub_local_api_server.local_api_socket_path,
        "nope",
        serde_json::Map::new(),
    )
    .unwrap();

    assert_eq!(exit_code, REFUSED_OPERATION_EXIT_CODE);
}

#[test]
fn params_must_be_one_json_object() {
    assert_eq!(
        operation_params_argument(r#"{"stream": "camera"}"#),
        Ok(params(json!({"stream": "camera"})))
    );
    assert_eq!(
        operation_params_argument("[1]"),
        Err("the params are one JSON object".to_owned())
    );
    assert!(
        operation_params_argument("{stream")
            .unwrap_err()
            .starts_with("not JSON: ")
    );
}
