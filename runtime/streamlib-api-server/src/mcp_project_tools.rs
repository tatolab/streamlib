// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The project tools: a project renamed, described and given a cover, each one transaction on
//! its history, and duplicated or deleted whole.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};
use streamlib_control_contract::{
    ControlOperation, ControlOperationCall, DeleteProject, DuplicateProject,
    DuplicateProjectRequest, OneProjectByItsDirectoryRequest, RenameProject, RenameProjectRequest,
    SetProjectCover, SetProjectCoverRequest, SetProjectDescription, SetProjectDescriptionRequest,
};

use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer};

#[tool_router(router = project_tool_router, vis = "pub(crate)")]
impl LocalApiMcpServerHandler {
    #[tool(description = RenameProject::AGENT_USAGE_NOTE)]
    async fn rename_project(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<RenameProjectRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::RenameProject(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = SetProjectDescription::AGENT_USAGE_NOTE)]
    async fn set_project_description(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<SetProjectDescriptionRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::SetProjectDescription(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = SetProjectCover::AGENT_USAGE_NOTE)]
    async fn set_project_cover(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<SetProjectCoverRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::SetProjectCover(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = DuplicateProject::AGENT_USAGE_NOTE)]
    async fn duplicate_project(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<DuplicateProjectRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::DuplicateProject(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = DeleteProject::AGENT_USAGE_NOTE)]
    async fn delete_project(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<OneProjectByItsDirectoryRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::DeleteProject(request),
            &tool_call_request_context,
        )
        .await
    }
}
