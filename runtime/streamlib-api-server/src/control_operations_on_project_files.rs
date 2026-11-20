// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The operations on a project's other files: listed, read, written, a node file created from
//! the template, and a node UI element created from the kit's.

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as STANDARD_BASE64;
use streamlib_project_repository::{
    AppliedProjectTransaction, NODE_UI_DIRECTORY, NODE_UI_MANIFEST_FILE_NAME, content_sha256_of,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, CreateNodeFileToolResult, CreateNodeUiElementToolResult,
    ListProjectFilesToolResult, ProjectFileOnTheWire, ProjectFileTransactionToolResult,
    ReadProjectFileToolResult,
};

use crate::control_operation_dispatcher::{
    ControlOperationDispatcher, ControlOperationRefusal, ControlRequestOrigin,
    the_author_of_a_change,
};
use crate::control_operations_on_project_assets::the_idempotency_key_a_caller_sent;
use crate::new_node_file_template::{
    NewNodeFileRenderedFromTheTemplate, render_the_new_node_file_template_for,
};
use crate::node_ui_element_template::{
    NodeUiElementTemplateRendered, the_node_ui_element_template_rendered,
};
use crate::refusal_code_and_place::RefusalCodeAndPlace;

use streamlib_control_contract::{
    CreateNodeFileRequest, CreateNodeUiElementRequest, ListProjectFilesRequest,
    ReadProjectFileRequest, WriteProjectFileRequest,
};
impl ControlOperationDispatcher {
    pub(crate) async fn list_project_files(
        &self,
        ListProjectFilesRequest {
            project_directory,
            under,
        }: ListProjectFilesRequest,
    ) -> Result<ListProjectFilesToolResult, ControlOperationRefusal> {
        let listed = self
            .a_project_repository_call_off_the_async_runtime(
                "list_project_files",
                project_directory,
                move |project_repository| project_repository.list_project_files(under.as_deref()),
            )
            .await?;
        Ok(ListProjectFilesToolResult {
            files: listed
                .into_iter()
                .map(|listed| ProjectFileOnTheWire {
                    path: listed.path,
                    byte_len: listed.byte_len,
                })
                .collect(),
        })
    }

    pub(crate) async fn read_project_file(
        &self,
        ReadProjectFileRequest {
            project_directory,
            path,
        }: ReadProjectFileRequest,
    ) -> Result<ReadProjectFileToolResult, ControlOperationRefusal> {
        let path_read = path.clone();
        let bytes = self
            .a_project_repository_call_off_the_async_runtime(
                "read_project_file",
                project_directory,
                move |project_repository| project_repository.read_project_file(&path_read),
            )
            .await?;
        Ok(ReadProjectFileToolResult {
            path,
            byte_len: bytes.len() as u64,
            content_base64: STANDARD_BASE64.encode(&bytes),
            content_sha256: content_sha256_of(&bytes),
        })
    }

    pub(crate) async fn create_node_file(
        &self,
        CreateNodeFileRequest {
            project_directory,
            path,
            author,
            idempotency_key,
        }: CreateNodeFileRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<CreateNodeFileToolResult, ControlOperationRefusal> {
        let NewNodeFileRenderedFromTheTemplate { node_type, content } =
            render_the_new_node_file_template_for(&path)
                .map_err(|why| format!("create_node_file failed: {why}"))?;
        let author = the_author_of_a_change(author, request_origin);
        let path_created = path.clone();
        let project_repositories = Arc::clone(&self.project_repositories);
        let AppliedProjectTransaction {
            transaction,
            commit,
            ..
        } = self
            .a_project_repository_call_off_the_async_runtime(
                "create_node_file",
                project_directory,
                move |project_repository| {
                    let idempotency_key = the_idempotency_key_a_caller_sent(idempotency_key)?;
                    let outcome = project_repository.create_project_file(
                        &author,
                        idempotency_key.as_ref(),
                        &path_created,
                        content.as_bytes(),
                    )?;
                    project_repositories
                        .announce_the_transaction_if_applied_now(project_repository, &outcome);
                    Ok(outcome.into_applied_transaction())
                },
            )
            .await?;
        Ok(CreateNodeFileToolResult {
            path,
            node_type,
            transaction: transaction.transaction.to_string(),
            commit,
        })
    }

    pub(crate) async fn create_node_ui_element(
        &self,
        CreateNodeUiElementRequest {
            project_directory,
            element,
            script,
            author,
            idempotency_key,
        }: CreateNodeUiElementRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<CreateNodeUiElementToolResult, ControlOperationRefusal> {
        let script_path = format!("{NODE_UI_DIRECTORY}/{script}");
        let manifest_path = format!("{NODE_UI_DIRECTORY}/{NODE_UI_MANIFEST_FILE_NAME}");
        let key_sent_again = idempotency_key.clone();
        if let Some(AppliedProjectTransaction {
            transaction,
            commit,
            ..
        }) = self
            .a_project_repository_call_off_the_async_runtime(
                "create_node_ui_element",
                project_directory.clone(),
                move |project_repository| {
                    project_repository.the_transaction_applied_under_a_key_the_caller_sent(
                        the_idempotency_key_a_caller_sent(key_sent_again)?.as_ref(),
                    )
                },
            )
            .await?
        {
            return Ok(CreateNodeUiElementToolResult {
                script_path,
                manifest_path,
                template_from: None,
                transaction: transaction.transaction.to_string(),
                commit,
            });
        }
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        let template_project_directory = project_directory.clone();
        let template_element = element.clone();
        let NodeUiElementTemplateRendered {
            template_from,
            content,
        } = tokio::task::spawn_blocking(move || {
            the_node_ui_element_template_rendered(
                operations_on_the_loaded_streams.as_ref(),
                &template_project_directory,
                &template_element,
            )
        })
        .await
        .map_err(ControlOperationRefusal::the_task_did_not_finish(
            "create_node_ui_element failed: reading the kit's template",
        ))?
        .map_err(|why| {
            ControlOperationRefusal::coded(
                RefusalCodeAndPlace::coded(ControlOperationRefusalCode::ExtensionSdkNotFound),
                format!("create_node_ui_element failed: {why}"),
            )
        })?;
        let author = the_author_of_a_change(author, request_origin);
        let project_repositories = Arc::clone(&self.project_repositories);
        let script_written = script.clone();
        let AppliedProjectTransaction {
            transaction,
            commit,
            ..
        } = self
            .a_project_repository_call_off_the_async_runtime(
                "create_node_ui_element",
                project_directory,
                move |project_repository| {
                    let idempotency_key = the_idempotency_key_a_caller_sent(idempotency_key)?;
                    let outcome = project_repository.create_node_ui_element(
                        &author,
                        idempotency_key.as_ref(),
                        &element,
                        &script_written,
                        content.as_bytes(),
                    )?;
                    project_repositories
                        .announce_the_transaction_if_applied_now(project_repository, &outcome);
                    Ok(outcome.into_applied_transaction())
                },
            )
            .await?;
        Ok(CreateNodeUiElementToolResult {
            script_path,
            manifest_path,
            template_from: Some(template_from),
            transaction: transaction.transaction.to_string(),
            commit,
        })
    }

    pub(crate) async fn write_project_file(
        &self,
        WriteProjectFileRequest {
            project_directory,
            path,
            content,
            content_base64,
            expected_sha256,
            author,
            idempotency_key,
        }: WriteProjectFileRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ProjectFileTransactionToolResult, ControlOperationRefusal> {
        let bytes = match (content, content_base64) {
            (Some(text), None) => text.into_bytes(),
            (None, Some(encoded)) => STANDARD_BASE64.decode(encoded).map_err(|why| {
                format!("write_project_file failed: `content_base64` is not base64: {why}")
            })?,
            _ => {
                return Err(
                    "write_project_file failed: send exactly one of `content` and \
                     `content_base64`"
                        .to_owned()
                        .into(),
                );
            }
        };
        let content_sha256 = content_sha256_of(&bytes);
        let author = the_author_of_a_change(author, request_origin);
        let path_written = path.clone();
        let project_repositories = Arc::clone(&self.project_repositories);
        let AppliedProjectTransaction {
            transaction,
            commit,
            ..
        } = self
            .a_project_repository_call_off_the_async_runtime(
                "write_project_file",
                project_directory,
                move |project_repository| {
                    let idempotency_key = the_idempotency_key_a_caller_sent(idempotency_key)?;
                    let outcome = project_repository.write_project_file(
                        &author,
                        idempotency_key.as_ref(),
                        &path_written,
                        &bytes,
                        expected_sha256.as_deref(),
                    )?;
                    project_repositories
                        .announce_the_transaction_if_applied_now(project_repository, &outcome);
                    Ok(outcome.into_applied_transaction())
                },
            )
            .await?;
        Ok(ProjectFileTransactionToolResult {
            path,
            content_sha256,
            transaction: transaction.transaction.to_string(),
            commit,
        })
    }
}
