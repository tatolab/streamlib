// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The node type listing: what a project, its imports and its installed
//! distributions declare, read from source without running any of it.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};
use streamlib_control_contract::{
    ControlOperation, ControlOperationCall, ListProjectNodeTypes, ListProjectNodeTypesRequest,
};

use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer};

#[tool_router(router = project_node_type_tool_router, vis = "pub(crate)")]
impl LocalApiMcpServerHandler {
    #[tool(description = ListProjectNodeTypes::AGENT_USAGE_NOTE)]
    async fn list_project_node_types(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<ListProjectNodeTypesRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::ListProjectNodeTypes(request),
            &tool_call_request_context,
        )
        .await
    }
}
