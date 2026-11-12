// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The graph tools: every edit of a stream's graph a transaction, committed to the graph
//! document and the project's history or applied live to the running stream alone, its nodes
//! by UUID, and each author's undo and redo.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};
use serde_json::json;
use streamlib_control_contract::{
    AddGroup, AddGroupRequest, AddNode, AddNodeRequest, AddNote, AddNoteRequest,
    CollapseSelectionIntoSubgraph, CollapseSelectionIntoSubgraphRequest, Connect, ControlOperation,
    ControlOperationCall, Disconnect, ImportGraphGenerator, ImportGraphGeneratorRequest,
    OneLinkGraphEditRequest, OneNodeGraphEditRequest, Redo, RefreshFragments,
    RefreshFragmentsRequest, RemoveGroup, RemoveGroupRequest, RemoveNode, RemoveNote,
    RemoveNoteRequest, RenameNode, RenameNodeRequest, SetExposedConfigFields,
    SetExposedConfigFieldsRequest, SetGroupFields, SetGroupFieldsRequest, SetNodeConfig,
    SetNodeConfigRequest, SetNoteFields, SetNoteFieldsRequest, SetPinValue, SetPinValueRequest,
    SetStudioFields, SetStudioFieldsRequest, Transact, TransactRequest, Undo, UndoOrRedoRequest,
    UnlockGeneratorFragment, UnlockGeneratorFragmentRequest,
};
use streamlib_runtime_client_contract::local_api_wire_contract::ProjectActionRefusalOnTheWire;

use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer};

#[tool_router(router = stream_graph_tool_router, vis = "pub(crate)")]
impl LocalApiMcpServerHandler {
    #[tool(description = Transact::AGENT_USAGE_NOTE)]
    async fn transact(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<TransactRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::Transact(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = ImportGraphGenerator::AGENT_USAGE_NOTE)]
    async fn import_graph_generator(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<ImportGraphGeneratorRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::ImportGraphGenerator(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = AddNode::AGENT_USAGE_NOTE)]
    async fn add_node(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<AddNodeRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::AddNode(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = RemoveNode::AGENT_USAGE_NOTE)]
    async fn remove_node(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<OneNodeGraphEditRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::RemoveNode(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = Connect::AGENT_USAGE_NOTE)]
    async fn connect(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<OneLinkGraphEditRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::Connect(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = Disconnect::AGENT_USAGE_NOTE)]
    async fn disconnect(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<OneLinkGraphEditRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::Disconnect(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = SetNodeConfig::AGENT_USAGE_NOTE)]
    async fn set_node_config(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<SetNodeConfigRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::SetNodeConfig(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = SetPinValue::AGENT_USAGE_NOTE)]
    async fn set_pin_value(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<SetPinValueRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::SetPinValue(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = RenameNode::AGENT_USAGE_NOTE)]
    async fn rename_node(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<RenameNodeRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::RenameNode(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = SetStudioFields::AGENT_USAGE_NOTE)]
    async fn set_studio_fields(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<SetStudioFieldsRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::SetStudioFields(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = SetExposedConfigFields::AGENT_USAGE_NOTE)]
    async fn set_exposed_config_fields(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<SetExposedConfigFieldsRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::SetExposedConfigFields(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = AddGroup::AGENT_USAGE_NOTE)]
    async fn add_group(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<AddGroupRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::AddGroup(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = SetGroupFields::AGENT_USAGE_NOTE)]
    async fn set_group_fields(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<SetGroupFieldsRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::SetGroupFields(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = CollapseSelectionIntoSubgraph::AGENT_USAGE_NOTE)]
    async fn collapse_selection_into_subgraph(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<CollapseSelectionIntoSubgraphRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::CollapseSelectionIntoSubgraph(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = UnlockGeneratorFragment::AGENT_USAGE_NOTE)]
    async fn unlock_generator_fragment(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<UnlockGeneratorFragmentRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::UnlockGeneratorFragment(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = AddNote::AGENT_USAGE_NOTE)]
    async fn add_note(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<AddNoteRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::AddNote(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = SetNoteFields::AGENT_USAGE_NOTE)]
    async fn set_note_fields(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<SetNoteFieldsRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::SetNoteFields(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = RemoveNote::AGENT_USAGE_NOTE)]
    async fn remove_note(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<RemoveNoteRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::RemoveNote(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = RemoveGroup::AGENT_USAGE_NOTE)]
    async fn remove_group(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<RemoveGroupRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::RemoveGroup(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = RefreshFragments::AGENT_USAGE_NOTE)]
    async fn refresh_fragments(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<RefreshFragmentsRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::RefreshFragments(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = Undo::AGENT_USAGE_NOTE)]
    async fn undo(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<UndoOrRedoRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::Undo(request),
            &tool_call_request_context,
        )
        .await
    }

    #[tool(description = Redo::AGENT_USAGE_NOTE)]
    async fn redo(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(request): Parameters<UndoOrRedoRequest>,
    ) -> ToolCallAnswer {
        self.a_dispatched_control_operation(
            ControlOperationCall::Redo(request),
            &tool_call_request_context,
        )
        .await
    }
}

/// `refusal` as its tool answers it: an error result whose text is the runtime's words and whose
/// `structuredContent` holds the refusal under `refusal`.
pub(crate) fn a_tool_result_of_the_refusal(
    refusal: ProjectActionRefusalOnTheWire,
) -> CallToolResult {
    let text = format!("{} failed: {}", refusal.action, refusal.runtime_words);
    let mut tool_result = CallToolResult::error(vec![ContentBlock::text(text)]);
    tool_result.structured_content = Some(json!({ "refusal": refusal }));
    tool_result
}
