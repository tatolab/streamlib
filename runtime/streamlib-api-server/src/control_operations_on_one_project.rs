// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The operations on a project as a whole: renamed, described and given a cover, each one
//! transaction on its history, and duplicated or deleted whole.

use std::path::PathBuf;
use std::sync::Arc;

use streamlib_control_contract::{
    DuplicateProjectRequest, OneProjectByItsDirectoryRequest, RenameProjectRequest,
    SetProjectCoverRequest, SetProjectDescriptionRequest,
};
use streamlib_project_repository::{
    AppliedProjectTransaction, ProjectRepositoryRefusal, ProjectSettingsAction,
    ProjectTransactionAuthor,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    DeleteProjectToolResult, DuplicateProjectToolResult, ProjectSettingsTransactionToolResult,
};
use uuid::Uuid;

use crate::control_operation_dispatcher::{
    ControlOperationDispatcher, ControlOperationRefusal, ControlRequestOrigin,
    the_author_of_a_change,
};
use crate::control_operations_on_project_assets::the_idempotency_key_a_caller_sent;

impl ControlOperationDispatcher {
    pub(crate) async fn rename_project(
        &self,
        RenameProjectRequest {
            project_directory,
            name,
            author,
            idempotency_key,
        }: RenameProjectRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ProjectSettingsTransactionToolResult, ControlOperationRefusal> {
        self.one_project_settings_change_as_a_transaction(
            "rename_project",
            project_directory,
            the_author_of_a_change(author, request_origin),
            idempotency_key,
            ProjectSettingsAction::SetProjectName { name },
        )
        .await
    }

    pub(crate) async fn set_project_description(
        &self,
        SetProjectDescriptionRequest {
            project_directory,
            description,
            author,
            idempotency_key,
        }: SetProjectDescriptionRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ProjectSettingsTransactionToolResult, ControlOperationRefusal> {
        let description = description.filter(|description| !description.trim().is_empty());
        self.one_project_settings_change_as_a_transaction(
            "set_project_description",
            project_directory,
            the_author_of_a_change(author, request_origin),
            idempotency_key,
            ProjectSettingsAction::SetProjectDescription { description },
        )
        .await
    }

    pub(crate) async fn set_project_cover(
        &self,
        SetProjectCoverRequest {
            project_directory,
            cover,
            author,
            idempotency_key,
        }: SetProjectCoverRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ProjectSettingsTransactionToolResult, ControlOperationRefusal> {
        let cover = cover
            .map(|cover| {
                Uuid::parse_str(&cover).map_err(|_| {
                    ControlOperationRefusal::Failed(format!(
                        "set_project_cover failed: `cover` must be an asset's UUID; got `{cover}`"
                    ))
                })
            })
            .transpose()?;
        self.one_project_settings_change_as_a_transaction(
            "set_project_cover",
            project_directory,
            the_author_of_a_change(author, request_origin),
            idempotency_key,
            ProjectSettingsAction::SetProjectCover { cover },
        )
        .await
    }

    pub(crate) async fn duplicate_project(
        &self,
        DuplicateProjectRequest {
            project_directory,
            into,
            author,
        }: DuplicateProjectRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<DuplicateProjectToolResult, ControlOperationRefusal> {
        let author = the_author_of_a_change(author, request_origin);
        self.a_project_repository_call_off_the_async_runtime(
            "duplicate_project",
            project_directory,
            move |project_repository| {
                let duplicate = project_repository.duplicate_into(&into, &author)?;
                Ok(DuplicateProjectToolResult {
                    project_directory: duplicate.project_directory().to_path_buf(),
                    project: duplicate.project_file().project.to_string(),
                    project_name: duplicate.project_file().name.clone(),
                    commit: duplicate.history().head_commit()?.unwrap_or_default(),
                })
            },
        )
        .await
    }

    pub(crate) async fn delete_project(
        &self,
        OneProjectByItsDirectoryRequest { project_directory }: OneProjectByItsDirectoryRequest,
    ) -> Result<DeleteProjectToolResult, ControlOperationRefusal> {
        let project_repositories = Arc::clone(&self.project_repositories);
        let queued = self
            .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                let queued = operations_on_the_loaded_streams.delete_project(&project_directory)?;
                project_repositories.close_the_project_repository_at(&queued.project_directory);
                Ok(queued)
            })
            .await?;
        Ok(DeleteProjectToolResult {
            action: queued.action_id,
            project_directory: queued.project_directory,
        })
    }

    async fn one_project_settings_change_as_a_transaction(
        &self,
        method_name: &'static str,
        project_directory: PathBuf,
        author: ProjectTransactionAuthor,
        idempotency_key: Option<String>,
        action: ProjectSettingsAction,
    ) -> Result<ProjectSettingsTransactionToolResult, ControlOperationRefusal> {
        let project_repositories = Arc::clone(&self.project_repositories);
        self.a_project_repository_call_off_the_async_runtime(
            method_name,
            project_directory,
            move |project_repository| {
                let idempotency_key = the_idempotency_key_a_caller_sent(idempotency_key)?;
                let outcome = project_repository
                    .apply_project_settings_action(&author, idempotency_key.as_ref(), action)
                    .inspect_err(|_| {
                        project_repositories
                            .announce_a_refused_action(project_repository.project_file().project);
                    })?;
                project_repositories
                    .announce_the_transaction_if_applied_now(project_repository, &outcome);
                let AppliedProjectTransaction {
                    transaction,
                    commit,
                    ..
                } = outcome.into_applied_transaction();
                let project_file = project_repository.project_file();
                Ok::<_, ProjectRepositoryRefusal>(ProjectSettingsTransactionToolResult {
                    project: project_file.project.to_string(),
                    project_name: project_file.name.clone(),
                    project_description: project_file.description.clone(),
                    project_cover: project_file.cover.map(|cover| cover.to_string()),
                    transaction: transaction.transaction.to_string(),
                    commit,
                })
            },
        )
        .await
    }
}
