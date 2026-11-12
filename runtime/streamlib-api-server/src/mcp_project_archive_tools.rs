// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The project archive tool: a new project unpacked from a `.zip` of one on this machine.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};
use streamlib_control_contract::{
    ControlOperation, ControlOperationCall, ImportProject, ImportProjectRequest,
};

use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer};

#[tool_router(router = project_archive_tool_router, vis = "pub(crate)")]
impl LocalApiMcpServerHandler {
    #[tool(description = ImportProject::AGENT_USAGE_NOTE)]
    async fn import_project(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<ImportProjectRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::ImportProject(request),
            &tool_call_request_context,
        )
        .await
    }
}
