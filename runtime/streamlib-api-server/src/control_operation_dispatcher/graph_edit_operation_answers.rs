// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_control_contract::{
    AddGroupRequest, AddNodeRequest, AddNoteRequest, CollapseSelectionIntoSubgraphRequest,
    ControlOperation, ImportGraphGeneratorRequest, OneLinkGraphEditRequest,
    OneNodeGraphEditRequest, RemoveGroupRequest, RemoveNoteRequest, RenameNodeRequest,
    SetGroupFieldsRequest, SetNodeInputValue, SetNodeInputValueRequest, SetNoteFieldsRequest,
    SetStudioFieldsRequest, TransactRequest, UnlockSubgraphRequest,
};
use streamlib_project_repository::{GRAPH_INTERFACE_VALUE_PORT_NAME, StreamGraphAction};

use super::{
    ControlOperationAnswer, ControlOperationDispatcher, ControlOperationRefusal,
    ControlRequestOrigin,
};

impl ControlOperationDispatcher {
    pub(super) async fn answer_transact(
        &self,
        transact_request: TransactRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let TransactRequest {
            operations,
            mode,
            author,
            idempotency_key,
        } = transact_request;
        self.apply_a_graph_transaction(
            "transact",
            operations,
            mode,
            author,
            idempotency_key,
            request_origin,
        )
        .await
        .map(ControlOperationAnswer::GraphTransaction)
    }

    pub(super) async fn answer_add_node(
        &self,
        add_node_request: AddNodeRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let AddNodeRequest {
            stream,
            level,
            node_type,
            name,
            studio,
            mode,
            author,
            idempotency_key,
        } = add_node_request;
        self.apply_a_graph_transaction(
            "add_node",
            vec![StreamGraphAction::AddNode {
                stream,
                level: level.unwrap_or_default(),
                node_type,
                name,
                studio,
                label: None,
            }],
            mode,
            author,
            idempotency_key,
            request_origin,
        )
        .await
        .map(ControlOperationAnswer::GraphTransaction)
    }

    pub(super) async fn answer_connect(
        &self,
        connect_request: OneLinkGraphEditRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let OneLinkGraphEditRequest {
            stream,
            level,
            source,
            target,
            mode,
            author,
            idempotency_key,
        } = connect_request;
        self.apply_a_graph_transaction(
            "connect",
            vec![StreamGraphAction::Connect {
                stream,
                level: level.unwrap_or_default(),
                source,
                target,
            }],
            mode,
            author,
            idempotency_key,
            request_origin,
        )
        .await
        .map(ControlOperationAnswer::GraphTransaction)
    }

    pub(super) async fn answer_import_graph_generator(
        &self,
        import_graph_generator_request: ImportGraphGeneratorRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let ImportGraphGeneratorRequest {
            stream,
            generator,
            name,
            mode,
            author,
            idempotency_key,
        } = import_graph_generator_request;
        self.apply_a_graph_transaction(
            "import_graph_generator",
            vec![StreamGraphAction::ImportGraphGenerator {
                stream,
                generator,
                name,
                label: None,
            }],
            mode,
            author,
            idempotency_key,
            request_origin,
        )
        .await
        .map(ControlOperationAnswer::GraphTransaction)
    }

    pub(super) async fn answer_remove_node(
        &self,
        remove_node_request: OneNodeGraphEditRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let OneNodeGraphEditRequest {
            stream,
            level,
            node,
            mode,
            author,
            idempotency_key,
        } = remove_node_request;
        self.apply_a_graph_transaction(
            "remove_node",
            vec![StreamGraphAction::RemoveNode {
                stream,
                level: level.unwrap_or_default(),
                node,
            }],
            mode,
            author,
            idempotency_key,
            request_origin,
        )
        .await
        .map(ControlOperationAnswer::GraphTransaction)
    }

    pub(super) async fn answer_disconnect(
        &self,
        disconnect_request: OneLinkGraphEditRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let OneLinkGraphEditRequest {
            stream,
            level,
            source,
            target,
            mode,
            author,
            idempotency_key,
        } = disconnect_request;
        self.apply_a_graph_transaction(
            "disconnect",
            vec![StreamGraphAction::Disconnect {
                stream,
                level: level.unwrap_or_default(),
                source,
                target,
            }],
            mode,
            author,
            idempotency_key,
            request_origin,
        )
        .await
        .map(ControlOperationAnswer::GraphTransaction)
    }

    pub(super) async fn answer_set_node_input_value(
        &self,
        set_node_input_value_request: SetNodeInputValueRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let SetNodeInputValueRequest {
            stream,
            level,
            node,
            input,
            member,
            value,
            mode,
            author,
            idempotency_key,
        } = set_node_input_value_request;
        const ACTION_NAME: &str = <SetNodeInputValue as ControlOperation>::METHOD_NAME;
        let level = level.unwrap_or_default();
        let (node, input) = match node {
            Some(node) => (node, input),
            None => (
                self.the_node_a_set_naming_no_node_sets(ACTION_NAME, &stream, &level, &input)
                    .await?,
                GRAPH_INTERFACE_VALUE_PORT_NAME.to_owned(),
            ),
        };
        self.apply_a_graph_transaction(
            ACTION_NAME,
            vec![StreamGraphAction::SetNodeInputValue {
                stream,
                level,
                node,
                input,
                member,
                value,
            }],
            mode,
            author,
            idempotency_key,
            request_origin,
        )
        .await
        .map(ControlOperationAnswer::GraphTransaction)
    }

    pub(super) async fn answer_rename_node(
        &self,
        rename_node_request: RenameNodeRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let RenameNodeRequest {
            stream,
            level,
            node,
            name,
            mode,
            author,
            idempotency_key,
        } = rename_node_request;
        self.apply_a_graph_transaction(
            "rename_node",
            vec![StreamGraphAction::RenameNode {
                stream,
                level: level.unwrap_or_default(),
                node,
                name,
            }],
            mode,
            author,
            idempotency_key,
            request_origin,
        )
        .await
        .map(ControlOperationAnswer::GraphTransaction)
    }

    pub(super) async fn answer_set_studio_fields(
        &self,
        set_studio_fields_request: SetStudioFieldsRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let SetStudioFieldsRequest {
            stream,
            level,
            node,
            studio,
            mode,
            author,
            idempotency_key,
        } = set_studio_fields_request;
        self.apply_a_graph_transaction(
            "set_studio_fields",
            vec![StreamGraphAction::SetStudioFields {
                stream,
                level: level.unwrap_or_default(),
                node,
                studio,
            }],
            mode,
            author,
            idempotency_key,
            request_origin,
        )
        .await
        .map(ControlOperationAnswer::GraphTransaction)
    }

    pub(super) async fn answer_add_group(
        &self,
        add_group_request: AddGroupRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let AddGroupRequest {
            stream,
            level,
            title,
            description,
            colour,
            nodes,
            mode,
            author,
            idempotency_key,
        } = add_group_request;
        self.apply_a_graph_transaction(
            "add_group",
            vec![StreamGraphAction::AddGroup {
                stream,
                level: level.unwrap_or_default(),
                title,
                description,
                colour,
                nodes,
            }],
            mode,
            author,
            idempotency_key,
            request_origin,
        )
        .await
        .map(ControlOperationAnswer::GraphTransaction)
    }

    pub(super) async fn answer_set_group_fields(
        &self,
        set_group_fields_request: SetGroupFieldsRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let SetGroupFieldsRequest {
            stream,
            level,
            group,
            title,
            description,
            colour,
            mode,
            author,
            idempotency_key,
        } = set_group_fields_request;
        self.apply_a_graph_transaction(
            "set_group_fields",
            vec![StreamGraphAction::SetGroupFields {
                stream,
                level: level.unwrap_or_default(),
                group,
                title,
                description,
                colour,
            }],
            mode,
            author,
            idempotency_key,
            request_origin,
        )
        .await
        .map(ControlOperationAnswer::GraphTransaction)
    }

    pub(super) async fn answer_collapse_selection_into_subgraph(
        &self,
        collapse_selection_into_subgraph_request: CollapseSelectionIntoSubgraphRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let CollapseSelectionIntoSubgraphRequest {
            stream,
            level,
            nodes,
            name,
            mode,
            author,
            idempotency_key,
        } = collapse_selection_into_subgraph_request;
        self.apply_a_graph_transaction(
            "collapse_selection_into_subgraph",
            vec![StreamGraphAction::CollapseSelectionIntoSubgraph {
                stream,
                level: level.unwrap_or_default(),
                nodes,
                name,
                label: None,
            }],
            mode,
            author,
            idempotency_key,
            request_origin,
        )
        .await
        .map(ControlOperationAnswer::GraphTransaction)
    }

    pub(super) async fn answer_unlock_subgraph(
        &self,
        unlock_subgraph_request: UnlockSubgraphRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let UnlockSubgraphRequest {
            stream,
            level,
            fragment,
            mode,
            author,
            idempotency_key,
        } = unlock_subgraph_request;
        self.apply_a_graph_transaction(
            "unlock_subgraph",
            vec![StreamGraphAction::UnlockSubgraph {
                stream,
                level: level.unwrap_or_default(),
                fragment,
            }],
            mode,
            author,
            idempotency_key,
            request_origin,
        )
        .await
        .map(ControlOperationAnswer::GraphTransaction)
    }

    pub(super) async fn answer_add_note(
        &self,
        add_note_request: AddNoteRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let AddNoteRequest {
            stream,
            text,
            position,
            size,
            mode,
            author,
            idempotency_key,
        } = add_note_request;
        self.apply_a_graph_transaction(
            "add_note",
            vec![StreamGraphAction::AddNote {
                stream,
                text,
                position,
                size,
            }],
            mode,
            author,
            idempotency_key,
            request_origin,
        )
        .await
        .map(ControlOperationAnswer::GraphTransaction)
    }

    pub(super) async fn answer_set_note_fields(
        &self,
        set_note_fields_request: SetNoteFieldsRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let SetNoteFieldsRequest {
            stream,
            note,
            text,
            position,
            size,
            mode,
            author,
            idempotency_key,
        } = set_note_fields_request;
        self.apply_a_graph_transaction(
            "set_note_fields",
            vec![StreamGraphAction::SetNoteFields {
                stream,
                note,
                text,
                position,
                size,
            }],
            mode,
            author,
            idempotency_key,
            request_origin,
        )
        .await
        .map(ControlOperationAnswer::GraphTransaction)
    }

    pub(super) async fn answer_remove_note(
        &self,
        remove_note_request: RemoveNoteRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let RemoveNoteRequest {
            stream,
            note,
            mode,
            author,
            idempotency_key,
        } = remove_note_request;
        self.apply_a_graph_transaction(
            "remove_note",
            vec![StreamGraphAction::RemoveNote { stream, note }],
            mode,
            author,
            idempotency_key,
            request_origin,
        )
        .await
        .map(ControlOperationAnswer::GraphTransaction)
    }

    pub(super) async fn answer_remove_group(
        &self,
        remove_group_request: RemoveGroupRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ControlOperationAnswer, ControlOperationRefusal> {
        let RemoveGroupRequest {
            stream,
            level,
            group,
            mode,
            author,
            idempotency_key,
        } = remove_group_request;
        self.apply_a_graph_transaction(
            "remove_group",
            vec![StreamGraphAction::RemoveGroup {
                stream,
                level: level.unwrap_or_default(),
                group,
            }],
            mode,
            author,
            idempotency_key,
            request_origin,
        )
        .await
        .map(ControlOperationAnswer::GraphTransaction)
    }
}
