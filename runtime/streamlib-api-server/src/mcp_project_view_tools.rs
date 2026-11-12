// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The view listing: every Tato Studio view a project and its imports bring in their `views/`.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};
use streamlib_control_contract::{
    ControlOperation, ControlOperationCall, ListProjectViews, ListProjectViewsRequest,
    WriteGraphInput, WriteGraphInputRequest, WriteStudioViewOutput, WriteStudioViewOutputRequest,
};

use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer};

#[tool_router(router = project_view_tool_router, vis = "pub(crate)")]
impl LocalApiMcpServerHandler {
    #[tool(description = ListProjectViews::AGENT_USAGE_NOTE)]
    async fn list_project_views(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<ListProjectViewsRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::ListProjectViews(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = WriteStudioViewOutput::AGENT_USAGE_NOTE)]
    async fn write_studio_view_output(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<WriteStudioViewOutputRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::WriteStudioViewOutput(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = WriteGraphInput::AGENT_USAGE_NOTE)]
    async fn write_graph_input(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<WriteGraphInputRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::WriteGraphInput(request),
            &tool_call_request_context,
        )
        .await
    }
}
