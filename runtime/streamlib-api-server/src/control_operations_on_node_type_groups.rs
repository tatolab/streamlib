// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `set_node_type_group`: a node type filed under a group of the project's own, or put back
//! where an import or its author files it, as one transaction on the project's history.

use std::path::Path;

use streamlib_control_contract::{ControlOperation, SetNodeTypeGroup, SetNodeTypeGroupRequest};
use streamlib_project_repository::{AppliedProjectTransaction, ProjectSettingsAction};
use streamlib_runtime_client_contract::local_api_wire_contract::SetNodeTypeGroupToolResult;

use crate::control_operation_dispatcher::{
    ControlOperationDispatcher, ControlOperationRefusal, ControlRequestOrigin,
    the_author_of_a_change,
};
use crate::control_operations_on_node_types::{
    the_built_in_node_type, the_refusal_of_a_node_type_not_offered,
};
use crate::control_operations_on_project_assets::the_idempotency_key_a_caller_sent;

impl ControlOperationDispatcher {
    pub(crate) async fn set_node_type_group(
        &self,
        SetNodeTypeGroupRequest {
            project_directory,
            node_type,
            group,
            author,
            idempotency_key,
        }: SetNodeTypeGroupRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<SetNodeTypeGroupToolResult, ControlOperationRefusal> {
        let author = the_author_of_a_change(author, request_origin);
        // Clearing is never refused, so a filing outlives no node type it names; a call sent
        // again under its key answers what it applied the first time.
        if group.is_some()
            && !self
                .was_applied_under_the_key(&project_directory, idempotency_key.clone())
                .await?
        {
            self.refuse_a_node_type_neither_the_project_nor_the_runtime_offers(
                &project_directory,
                &node_type,
            )
            .await?;
        }
        self.one_project_settings_change_answered_by(
            SetNodeTypeGroup::METHOD_NAME,
            project_directory,
            author,
            idempotency_key,
            ProjectSettingsAction::SetNodeTypeGroup {
                node_type: node_type.clone(),
                group,
            },
            |project_file,
             AppliedProjectTransaction {
                 transaction,
                 commit,
                 ..
             }| {
                SetNodeTypeGroupToolResult {
                    project: project_file.project.to_string(),
                    group: project_file.node_type_groups.get(&node_type).cloned(),
                    node_type,
                    transaction: transaction.transaction.to_string(),
                    commit,
                }
            },
        )
        .await
    }

    async fn was_applied_under_the_key(
        &self,
        project_directory: &Path,
        idempotency_key: Option<String>,
    ) -> Result<bool, ControlOperationRefusal> {
        if idempotency_key.is_none() {
            return Ok(false);
        }
        self.a_project_repository_call_off_the_async_runtime(
            SetNodeTypeGroup::METHOD_NAME,
            project_directory.to_path_buf(),
            move |project_repository| {
                let idempotency_key = the_idempotency_key_a_caller_sent(idempotency_key)?;
                Ok(project_repository
                    .the_transaction_applied_under_a_key_the_caller_sent(idempotency_key.as_ref())?
                    .is_some())
            },
        )
        .await
    }

    async fn refuse_a_node_type_neither_the_project_nor_the_runtime_offers(
        &self,
        project_directory: &Path,
        node_type: &str,
    ) -> Result<(), ControlOperationRefusal> {
        if the_built_in_node_type(node_type).is_some() {
            return Ok(());
        }
        let listing = self
            .the_node_types_the_project_offers_read_from_source(
                SetNodeTypeGroup::METHOD_NAME,
                project_directory.to_path_buf(),
            )
            .await?;
        let is_offered = listing
            .sources
            .iter()
            .flat_map(|source| &source.node_types)
            .any(|read| read.node_type == node_type);
        if is_offered {
            Ok(())
        } else {
            Err(the_refusal_of_a_node_type_not_offered(
                SetNodeTypeGroup::METHOD_NAME,
                &listing,
                node_type,
            ))
        }
    }
}
