// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The MCP tools, generated from the control operation registry's agent surface: each listed
//! operation as a tool of its own, and the discovery tools that reach every other one.

use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::RoleServer;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, JsonObject, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use streamlib_control_contract::{
    AgentToolListed, CALL_OPERATION_TOOL_NAME, CallOperationRequest, ControlOperationCall,
    ControlOperationCallParseRefusal, ControlOperationEffect, DESCRIBE_OPERATIONS_TOOL_NAME,
    DescribeOperationsRequest, LIST_OPERATION_GROUPS_TOOL_NAME, ListOperationGroupsRequest,
    the_agent_tools_listed, the_operation_groups_served_to_agents,
    the_operation_served_to_agents_as, the_operations_described_for,
};
use streamlib_runtime_client_contract::local_api_wire_contract::ProjectActionRefusalOnTheWire;

use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer, serialized_tool_result};

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
        let arguments = Value::Object(arguments.unwrap_or_default());
        match tool_name.as_ref() {
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
            .and_then(|described| {
                serialized_tool_result(DESCRIBE_OPERATIONS_TOOL_NAME, &described)
            })),
            CALL_OPERATION_TOOL_NAME => {
                let CallOperationRequest { operation, params } =
                    match the_arguments_of(CALL_OPERATION_TOOL_NAME, arguments) {
                        Ok(request) => request,
                        Err(why) => return Ok(Err(why)),
                    };
                if the_operation_served_to_agents_as(&operation).is_none() {
                    return Ok(Err(format!(
                        "{CALL_OPERATION_TOOL_NAME} failed: the runtime serves no operation \
                         named `{operation}`; `{LIST_OPERATION_GROUPS_TOOL_NAME}` lists every one"
                    )));
                }
                self.the_answer_to_an_operation_an_agent_called(
                    &operation,
                    Value::Object(params.unwrap_or_default()),
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
                Some(_) => Ok(Err(format!(
                    "`{operation_name}` is an operation without a tool of its own: call \
                     `{CALL_OPERATION_TOOL_NAME}` with `operation` `{operation_name}` and its \
                     `params`, which `{DESCRIBE_OPERATIONS_TOOL_NAME}` states"
                ))),
                None => Err(McpError::invalid_params("tool not found", None)),
            },
        }
    }

    async fn the_answer_to_an_operation_an_agent_called(
        &self,
        operation_name: &str,
        params: Value,
        tool_call_request_context: &RequestContext<RoleServer>,
    ) -> Result<ToolCallAnswer, McpError> {
        match ControlOperationCall::parse(operation_name, Some(params)) {
            Ok(call) => Ok(self
                .a_dispatched_control_operation(call, tool_call_request_context)
                .await),
            Err(refusal @ ControlOperationCallParseRefusal::InvalidParams { .. }) => {
                Ok(Err(format!("{operation_name} failed: {refusal}")))
            }
            Err(ControlOperationCallParseRefusal::NoSuchMethod { .. }) => {
                Err(McpError::invalid_params("tool not found", None))
            }
        }
    }
}

fn the_arguments_of<Arguments: DeserializeOwned>(
    tool_name: &str,
    arguments: Value,
) -> Result<Arguments, String> {
    serde_json::from_value(arguments).map_err(|parse_failure| {
        format!("{tool_name} failed: its arguments do not match its input schema: {parse_failure}")
    })
}

/// A refused project action as a tool answers it: the runtime's words as the error text, and the
/// refusal itself under `structuredContent.refusal`.
pub(crate) fn a_tool_result_of_the_refusal(
    refusal: ProjectActionRefusalOnTheWire,
) -> CallToolResult {
    let text = format!("{} failed: {}", refusal.action, refusal.runtime_words);
    let mut tool_result = CallToolResult::error(vec![ContentBlock::text(text)]);
    tool_result.structured_content = Some(json!({ "refusal": refusal }));
    tool_result
}
