// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The file tools: a project's files listed, read, written, and a new node file created from
//! the runtime's node template, each write one transaction on
//! the project's history that an undo reverses.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};
use streamlib_control_contract::{
    ControlOperation, ControlOperationCall, CreateNodeFile, CreateNodeFileRequest,
    ListProjectFiles, ListProjectFilesRequest, ReadProjectFile, ReadProjectFileRequest,
    WriteProjectFile, WriteProjectFileRequest,
};

use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer};

#[tool_router(router = project_file_tool_router, vis = "pub(crate)")]
impl LocalApiMcpServerHandler {
    #[tool(description = ListProjectFiles::AGENT_USAGE_NOTE)]
    async fn list_project_files(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<ListProjectFilesRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::ListProjectFiles(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = ReadProjectFile::AGENT_USAGE_NOTE)]
    async fn read_project_file(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<ReadProjectFileRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::ReadProjectFile(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = CreateNodeFile::AGENT_USAGE_NOTE)]
    async fn create_node_file(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<CreateNodeFileRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::CreateNodeFile(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = WriteProjectFile::AGENT_USAGE_NOTE)]
    async fn write_project_file(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<WriteProjectFileRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::WriteProjectFile(request),
            &tool_call_request_context,
        )
        .await
    }
}
