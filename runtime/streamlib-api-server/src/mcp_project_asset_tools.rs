// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The asset tools: a project's assets listed, described, imported, renamed, moved, deleted and
//! given new settings, and its streams created, deleted and named, each change one transaction
//! on the project's history.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};
use streamlib_control_contract::{
    ControlOperation, ControlOperationCall, CreateStream, CreateStreamRequest, DeleteAsset,
    DeleteAssetRequest, DeleteStream, DeleteStreamRequest, DescribeAsset, DescribeAssetRequest,
    ImportAsset, ImportAssetRequest, ListAssets, ListAssetsRequest, ReadAssetPreview,
    ReadAssetPreviewRequest, RelocateAsset, RelocateAssetRequest, ReplaceAsset,
    ReplaceAssetRequest, SetAssetSettings, SetAssetSettingsRequest, SetStreamDisplayName,
    SetStreamDisplayNameRequest,
};

use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer};

#[tool_router(router = project_asset_tool_router, vis = "pub(crate)")]
impl LocalApiMcpServerHandler {
    #[tool(description = ListAssets::AGENT_USAGE_NOTE)]
    async fn list_assets(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<ListAssetsRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::ListAssets(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = DescribeAsset::AGENT_USAGE_NOTE)]
    async fn describe_asset(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<DescribeAssetRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::DescribeAsset(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = ReadAssetPreview::AGENT_USAGE_NOTE)]
    async fn read_asset_preview(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<ReadAssetPreviewRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::ReadAssetPreview(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = ImportAsset::AGENT_USAGE_NOTE)]
    async fn import_asset(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<ImportAssetRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::ImportAsset(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = RelocateAsset::AGENT_USAGE_NOTE)]
    async fn relocate_asset(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<RelocateAssetRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::RelocateAsset(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = DeleteAsset::AGENT_USAGE_NOTE)]
    async fn delete_asset(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<DeleteAssetRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::DeleteAsset(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = SetAssetSettings::AGENT_USAGE_NOTE)]
    async fn set_asset_settings(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<SetAssetSettingsRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::SetAssetSettings(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = ReplaceAsset::AGENT_USAGE_NOTE)]
    async fn replace_asset(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<ReplaceAssetRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::ReplaceAsset(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = CreateStream::AGENT_USAGE_NOTE)]
    async fn create_stream(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<CreateStreamRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::CreateStream(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = DeleteStream::AGENT_USAGE_NOTE)]
    async fn delete_stream(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<DeleteStreamRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::DeleteStream(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = SetStreamDisplayName::AGENT_USAGE_NOTE)]
    async fn set_stream_display_name(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<SetStreamDisplayNameRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::SetStreamDisplayName(request),
            &tool_call_request_context,
        )
        .await
    }
}
