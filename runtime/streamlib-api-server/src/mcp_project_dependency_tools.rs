// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The dependency tools: another project copied whole into this one's `imports/` folder, copied
//! again, listed and taken out, and its streams placed as fragments referencing them.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};
use streamlib_control_contract::{
    AddDependency, AddDependencyRequest, ControlOperation, ControlOperationCall, ListDependencies,
    ListDependenciesRequest, RemoveDependency, RemoveDependencyRequest, UpdateDependency,
    UpdateDependencyRequest, UseDependencyStream, UseDependencyStreamRequest,
};

use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer};

#[tool_router(router = project_dependency_tool_router, vis = "pub(crate)")]
impl LocalApiMcpServerHandler {
    #[tool(description = AddDependency::AGENT_USAGE_NOTE)]
    async fn add_dependency(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<AddDependencyRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::AddDependency(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = UpdateDependency::AGENT_USAGE_NOTE)]
    async fn update_dependency(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<UpdateDependencyRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::UpdateDependency(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = ListDependencies::AGENT_USAGE_NOTE)]
    async fn list_dependencies(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<ListDependenciesRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::ListDependencies(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = RemoveDependency::AGENT_USAGE_NOTE)]
    async fn remove_dependency(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<RemoveDependencyRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::RemoveDependency(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = UseDependencyStream::AGENT_USAGE_NOTE)]
    async fn use_dependency_stream(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<UseDependencyStreamRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::UseDependencyStream(request),
            &tool_call_request_context,
        )
        .await
    }
}
