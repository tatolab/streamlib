// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The MCP tools, generated from the control operation registry's agent surface: each listed
//! operation as a tool of its own, and the discovery tools that reach every other one.

use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::RoleServer;
use rmcp::model::{CallToolRequestParams, CallToolResult, JsonObject, Tool, ToolAnnotations};
use rmcp::service::RequestContext;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use streamlib_control_contract::{
    AgentToolListed, CALL_OPERATION_TOOL_NAME, CallOperationRequest, ControlOperationCall,
    ControlOperationCallParseRefusal, ControlOperationEffect, DESCRIBE_OPERATIONS_TOOL_NAME,
    DescribeOperationsRequest, LIST_OPERATION_GROUPS_TOOL_NAME, ListOperationGroupsRequest,
    ParamsThatDoNotMatchTheirSchema, the_agent_tools_listed, the_operation_groups_served_to_agents,
    the_operation_served_to_agents_as, the_operations_described_for, the_params_read_as,
    the_refusal_of_an_unknown_operation, the_suggested_call,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, ControlOperationRefusalOnTheWire,
};

use crate::mcp::{
    LocalApiMcpServerHandler, ToolCallAnswer, json_text_content_block, serialized_tool_result,
};

/// The tool list `tools/list` answers.
pub(crate) fn the_agent_tools_listed_over_mcp() -> Vec<Tool> {
    the_agent_tools_listed()
        .into_iter()
        .map(the_mcp_tool_of)
        .collect()
}

fn the_mcp_tool_of(listed: AgentToolListed) -> Tool {
    let mut tool = Tool::new(
        listed.tool_name,
        listed.description,
        Arc::new(the_json_object_of(listed.input_schema)),
    );
    tool.output_schema = listed
        .output_schema
        .map(|output_schema| Arc::new(the_json_object_of(output_schema)));
    tool.annotations = Some(match listed.effect {
        ControlOperationEffect::ReadsOnly => ToolAnnotations::new().read_only(true),
        ControlOperationEffect::Changes => {
            ToolAnnotations::new().read_only(false).destructive(false)
        }
        ControlOperationEffect::Destroys => {
            ToolAnnotations::new().read_only(false).destructive(true)
        }
    });
    tool
}

fn the_json_object_of(schema: Value) -> JsonObject {
    match schema {
        Value::Object(schema) => schema,
        _ => JsonObject::new(),
    }
}

impl LocalApiMcpServerHandler {
    /// What `tools/call` answers `request`: the tool's answer, or the protocol error that refuses
    /// a name the runtime serves nothing under.
    pub(crate) async fn the_answer_to_an_agent_tool_call(
        &self,
        CallToolRequestParams {
            name: tool_name,
            arguments,
            ..
        }: CallToolRequestParams,
        tool_call_request_context: &RequestContext<RoleServer>,
    ) -> Result<ToolCallAnswer, McpError> {
        let arguments =
            match the_answer_of_a_discovery_tool(&tool_name, arguments.unwrap_or_default()) {
                Ok(answer) => return Ok(answer),
                Err(arguments) => arguments,
            };
        match tool_name.as_ref() {
            CALL_OPERATION_TOOL_NAME => {
                let CallOperationRequest { operation, params } =
                    match the_arguments_of(CALL_OPERATION_TOOL_NAME, arguments) {
                        Ok(request) => request,
                        Err(refusal) => return Ok(Err(refusal)),
                    };
                let params =
                    match the_answer_of_a_discovery_tool(&operation, params.unwrap_or_default()) {
                        Ok(answer) => return Ok(answer),
                        Err(params) => params,
                    };
                if the_operation_served_to_agents_as(&operation).is_none() {
                    return Ok(Err(Box::new(the_refusal_of_an_unknown_operation(
                        Some(CALL_OPERATION_TOOL_NAME),
                        &operation,
                    ))));
                }
                self.the_answer_to_an_operation_an_agent_called(
                    &operation,
                    params,
                    tool_call_request_context,
                )
                .await
            }
            operation_name => match the_operation_served_to_agents_as(operation_name) {
                Some(declaration) if declaration.is_listed_as_an_agent_tool() => {
                    self.the_answer_to_an_operation_an_agent_called(
                        operation_name,
                        arguments,
                        tool_call_request_context,
                    )
                    .await
                }
                Some(_) => Ok(Err(Box::new(ControlOperationRefusalOnTheWire {
                    suggested_call: the_suggested_call(operation_name, arguments),
                    ..ControlOperationRefusalOnTheWire::of_the_action(
                        operation_name,
                        ControlOperationRefusalCode::Refused,
                        format!(
                            "`{operation_name}` is an operation without a tool of its own: call \
                             `{CALL_OPERATION_TOOL_NAME}` with `operation` `{operation_name}` and \
                             its `params`, which `{DESCRIBE_OPERATIONS_TOOL_NAME}` states"
                        ),
                    )
                }))),
                None => Err(McpError::invalid_params("tool not found", None)),
            },
        }
    }

    async fn the_answer_to_an_operation_an_agent_called(
        &self,
        operation_name: &str,
        params: Map<String, Value>,
        tool_call_request_context: &RequestContext<RoleServer>,
    ) -> Result<ToolCallAnswer, McpError> {
        match ControlOperationCall::parse(operation_name, Some(Value::Object(params))) {
            Ok(call) => Ok(self
                .a_dispatched_control_operation(call, tool_call_request_context)
                .await),
            Err(refusal @ ControlOperationCallParseRefusal::InvalidParams { .. }) => {
                Ok(Err(Box::new(refusal.the_refusal_on_the_wire())))
            }
            Err(ControlOperationCallParseRefusal::NoSuchMethod { .. }) => {
                Err(McpError::invalid_params("tool not found", None))
            }
        }
    }
}

/// What the discovery tool `tool_name` answers `arguments`, whether called by its name or through
/// `call_operation`; the `arguments` handed back when `tool_name` names no discovery tool that
/// answers without the dispatcher.
fn the_answer_of_a_discovery_tool(
    tool_name: &str,
    arguments: Map<String, Value>,
) -> Result<ToolCallAnswer, Map<String, Value>> {
    match tool_name {
        LIST_OPERATION_GROUPS_TOOL_NAME => Ok(the_arguments_of(
            LIST_OPERATION_GROUPS_TOOL_NAME,
            arguments,
        )
        .and_then(|ListOperationGroupsRequest {}| {
            serialized_tool_result(
                LIST_OPERATION_GROUPS_TOOL_NAME,
                &the_operation_groups_served_to_agents(),
            )
        })),
        DESCRIBE_OPERATIONS_TOOL_NAME => Ok(the_arguments_of::<DescribeOperationsRequest>(
            DESCRIBE_OPERATIONS_TOOL_NAME,
            arguments,
        )
        .and_then(the_operations_described_for)
        .and_then(|described| serialized_tool_result(DESCRIBE_OPERATIONS_TOOL_NAME, &described))),
        _ => Err(arguments),
    }
}

/// A discovery tool's `arguments` read as `Arguments`, or the refusal naming where they do not
/// match its input schema.
fn the_arguments_of<Arguments: DeserializeOwned>(
    tool_name: &str,
    arguments: Map<String, Value>,
) -> Result<Arguments, Box<ControlOperationRefusalOnTheWire>> {
    the_params_read_as(Value::Object(arguments)).map_err(
        |ParamsThatDoNotMatchTheirSchema { pointer, why }| {
            Box::new(ControlOperationRefusalOnTheWire {
                refused_params_pointer: pointer,
                ..ControlOperationRefusalOnTheWire::of_the_action(
                    tool_name,
                    ControlOperationRefusalCode::ParamsDoNotMatchSchema,
                    format!(
                        "{tool_name} failed: its arguments do not match its input schema: {why}"
                    ),
                )
            })
        },
    )
}

/// A refused call as a tool answers it: the refusal under `structuredContent.refusal`, and the
/// same refusal as the JSON of its text, so a host that passes only text loses nothing.
pub(crate) fn a_tool_result_of_the_refusal(
    refusal: ControlOperationRefusalOnTheWire,
) -> CallToolResult {
    let refusal = refusal.the_refusal_as_json();
    let mut tool_result = CallToolResult::error(vec![json_text_content_block(&refusal)]);
    tool_result.structured_content = Some(json!({ "refusal": refusal }));
    tool_result
}
