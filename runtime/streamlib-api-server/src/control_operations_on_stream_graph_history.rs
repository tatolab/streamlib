// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The operations that reverse a project's transactions and refresh its generated fragments:
//! undo, redo, and the fragments a changed module defines built again.

use std::path::PathBuf;
use std::sync::Arc;

use streamlib_control_contract::{GraphEditMode, RefreshFragmentsRequest, UndoOrRedoRequest};
use streamlib_project_repository::{
    ProjectRepository, ProjectRepositoryRefusal, ProjectTransactionAuthor,
    ProjectTransactionReversal,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    GraphTransactionToolResult, ProjectTransactionOnTheWire,
};

use crate::control_operation_dispatcher::{
    ControlOperationDispatcher, ControlOperationRefusal, ControlRequestOrigin,
    the_author_of_a_change,
};
use crate::control_operations_on_project_assets::the_idempotency_key_a_caller_sent;
use crate::mcp_project_change_feed::ProjectActionTheRuntimeRefused;
use crate::stream_graph_transaction_executor::{
    GraphTransactionRefusal, StreamGraphTransactionExecutor,
    the_refresh_of_each_fragment_a_changed_module_defines,
};

impl ControlOperationDispatcher {
    pub(crate) async fn refresh_fragments(
        &self,
        RefreshFragmentsRequest {
            project_directory,
            changed_files,
            mode,
            author,
            idempotency_key,
        }: RefreshFragmentsRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<GraphTransactionToolResult, ControlOperationRefusal> {
        if !project_directory.is_absolute() {
            return Err(format!(
                "refresh_fragments failed: `project_directory` must be an absolute path; got `{}`",
                project_directory.display()
            )
            .into());
        }
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        let project_repositories = Arc::clone(&self.project_repositories);
        let refreshes = tokio::task::spawn_blocking(move || {
            the_refresh_of_each_fragment_a_changed_module_defines(
                operations_on_the_loaded_streams.as_ref(),
                &project_repositories,
                &project_directory,
                &changed_files,
            )
        })
        .await
        .map_err(|join_failure| {
            format!("refresh_fragments failed: the refresh did not finish: {join_failure}")
        })?
        .map_err(|refusal| format!("refresh_fragments failed: {refusal}"))?;
        if refreshes.is_empty() {
            return Ok(GraphTransactionToolResult {
                mode: mode.unwrap_or_default().on_the_wire().to_owned(),
                transaction: None,
                commit: None,
                operations: Vec::new(),
                added_nodes: Vec::new(),
                connected_links: Vec::new(),
                added_groups: Vec::new(),
                added_notes: Vec::new(),
            });
        }
        self.apply_a_graph_transaction(
            "refresh_fragments",
            refreshes,
            mode,
            author,
            idempotency_key,
            request_origin,
        )
        .await
    }

    pub(crate) async fn undo(
        &self,
        UndoOrRedoRequest {
            project_directory,
            author,
            idempotency_key,
        }: UndoOrRedoRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ProjectTransactionOnTheWire, ControlOperationRefusal> {
        self.an_undo_or_redo(
            "undo",
            project_directory,
            the_author_of_a_change(author, request_origin),
            idempotency_key,
            ProjectRepository::undo_by,
        )
        .await
    }

    pub(crate) async fn redo(
        &self,
        UndoOrRedoRequest {
            project_directory,
            author,
            idempotency_key,
        }: UndoOrRedoRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ProjectTransactionOnTheWire, ControlOperationRefusal> {
        self.an_undo_or_redo(
            "redo",
            project_directory,
            the_author_of_a_change(author, request_origin),
            idempotency_key,
            ProjectRepository::redo_by,
        )
        .await
    }

    async fn an_undo_or_redo(
        &self,
        method_name: &'static str,
        project_directory: PathBuf,
        author: ProjectTransactionAuthor,
        idempotency_key: Option<String>,
        reversal_of: fn(
            &ProjectRepository,
            &ProjectTransactionAuthor,
        ) -> Result<ProjectTransactionReversal, ProjectRepositoryRefusal>,
    ) -> Result<ProjectTransactionOnTheWire, ControlOperationRefusal> {
        if !project_directory.is_absolute() {
            return Err(format!(
                "{method_name} failed: `project_directory` must be an absolute path; got `{}`",
                project_directory.display()
            )
            .into());
        }
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        let project_repositories = Arc::clone(&self.project_repositories);
        let project_actions_refused = Arc::clone(&self.project_actions_refused);
        let async_runtime = tokio::runtime::Handle::current();
        let applied = tokio::task::spawn_blocking(move || {
            let idempotency_key_as_sent = idempotency_key.clone();
            StreamGraphTransactionExecutor {
                operations_on_the_loaded_streams: operations_on_the_loaded_streams.as_ref(),
                project_repositories: &project_repositories,
                async_runtime: &async_runtime,
            }
            .apply_a_reversal(&project_directory, &author, idempotency_key, reversal_of)
            .map_err(|refusal| {
                let refusal = GraphTransactionRefusal::of_the_whole_transaction(refusal)
                    .into_the_refusal_on_the_wire(method_name);
                if let Ok(project_repository) =
                    project_repositories.the_project_repository_at(&project_directory)
                {
                    let project = project_repository.lock().project_file().project;
                    let idempotency_key =
                        the_idempotency_key_a_caller_sent(idempotency_key_as_sent)
                            .ok()
                            .flatten();
                    project_actions_refused.keep_and_announce(
                        &project_repositories,
                        project,
                        ProjectActionTheRuntimeRefused {
                            author: &author,
                            mode: GraphEditMode::Commit.on_the_wire(),
                            operations_as_requested: Vec::new(),
                            idempotency_key: idempotency_key.as_ref(),
                            refusal: refusal.clone(),
                        },
                    );
                }
                Box::new(refusal)
            })
        })
        .await
        .map_err(|join_failure| {
            format!("{method_name} failed: the transaction did not finish: {join_failure}")
        })?;
        applied.map_err(ControlOperationRefusal::RefusedAction)
    }
}
