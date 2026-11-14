// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The operations on a project's imports of other projects: added, updated, listed and removed,
//! and an imported project's stream placed in a graph.

use std::path::PathBuf;
use std::sync::Arc;

use streamlib::sdk::runtime::StreamActionState;
use tokio::sync::broadcast::error::RecvError;

use streamlib_control_contract::{
    AddDependencyRequest, ListDependenciesRequest, RemoveDependencyRequest,
    UpdateDependencyRequest, UseDependencyStreamRequest,
};
use streamlib_project_repository::{
    AppliedProjectTransaction, PYPROJECT_FILE_NAME, ProjectActionApplyOutcome, ProjectImport,
    ProjectImportOperation, ProjectOperation, ProjectRepositoriesOpenInThisRuntime,
    ProjectRepository, ProjectRepositoryOneWriterAtATime, ProjectRepositoryRefusal,
    StreamGraphAction,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    DependencyTransactionToolResult, ListDependenciesToolResult, ListedProjectDependencyOnTheWire,
    ProjectDependencyOnTheWire, UpdateDependencyToolResult,
};
use uuid::Uuid;

use crate::control_operation_dispatcher::{
    ControlOperationDispatcher, ControlOperationRefusal, ControlRequestOrigin,
    the_author_of_a_change,
};
use crate::control_operations_on_project_assets::the_idempotency_key_a_caller_sent;
use crate::stream_graph_transaction_executor::StreamGraphTransactionExecutor;
use streamlib_control_contract::GraphEditMode;

impl ControlOperationDispatcher {
    pub(crate) async fn add_dependency(
        &self,
        AddDependencyRequest {
            project_directory,
            source,
            folder,
            author,
            idempotency_key,
        }: AddDependencyRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<DependencyTransactionToolResult, ControlOperationRefusal> {
        let author = the_author_of_a_change(author, request_origin);
        let project_repositories = Arc::clone(&self.project_repositories);
        let answered = tokio::task::spawn_blocking(move || {
            let project_repository =
                the_project_repository_open_at(&project_repositories, &project_directory)?;
            let mut project_repository_held = project_repository.lock();
            let idempotency_key = the_idempotency_key_a_caller_sent(idempotency_key)
                .map_err(|refusal| refusal.to_string())?;
            let outcome = project_repository_held
                .add_dependency(
                    &author,
                    idempotency_key.as_ref(),
                    &source,
                    folder.as_deref(),
                )
                .map_err(|refusal| refusal.to_string())?;
            project_repositories
                .announce_the_transaction_if_applied_now(&project_repository_held, &outcome);
            let venv_to_make_current =
                VenvToMakeCurrent::once_applied_now(&project_repository_held, &outcome);
            let applied = outcome.into_applied_transaction();
            let added = the_import_the_transaction_placed(&applied)?;
            Ok::<_, String>((
                DependencyTransactionToolResult {
                    dependency: the_dependency_on_the_wire(&added),
                    transaction: applied.transaction.transaction.to_string(),
                    commit: applied.commit,
                },
                venv_to_make_current,
            ))
        })
        .await
        .map_err(|join_failure| {
            format!("add_dependency failed: the call did not finish: {join_failure}")
        })?
        .map_err(|refusal| format!("add_dependency failed: {refusal}"))?;
        let (answered, venv_to_make_current) = answered;
        self.make_the_venv_current_and_announce_it(venv_to_make_current);
        Ok(answered)
    }

    pub(crate) async fn update_dependency(
        &self,
        UpdateDependencyRequest {
            project_directory,
            project,
            source,
            overwrite_edited_files,
            author,
            idempotency_key,
        }: UpdateDependencyRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<UpdateDependencyToolResult, ControlOperationRefusal> {
        let author = the_author_of_a_change(author, request_origin);
        let project = the_import_named(&project)
            .map_err(|refusal| format!("update_dependency failed: {refusal}"))?;
        let overwrite_edited_files = overwrite_edited_files.unwrap_or(false);
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        let project_repositories = Arc::clone(&self.project_repositories);
        let async_runtime = tokio::runtime::Handle::current();
        let answered = tokio::task::spawn_blocking(move || {
            let project_repository =
                the_project_repository_open_at(&project_repositories, &project_directory)?;
            let mut project_repository_held = project_repository.lock();
            let idempotency_key = the_idempotency_key_a_caller_sent(idempotency_key)
                .map_err(|refusal| refusal.to_string())?;
            let files_the_update_overwrites = match project_repository_held.import(project) {
                Ok(held) if overwrite_edited_files => project_repository_held
                    .files_edited_in_the_import(&held)
                    .map_err(|refusal| refusal.to_string())?,
                _ => Vec::new(),
            };
            let outcome = match project_repository_held.update_dependency(
                &author,
                idempotency_key.as_ref(),
                project,
                source.as_deref(),
                overwrite_edited_files,
            ) {
                Ok(outcome) => outcome,
                Err(ProjectRepositoryRefusal::ImportEditedSinceItsCopy {
                    edited_files, ..
                }) => {
                    let held = project_repository_held
                        .import(project)
                        .map_err(|refusal| refusal.to_string())?;
                    return Ok::<_, String>((
                        UpdateDependencyToolResult {
                            dependency: the_dependency_on_the_wire(&held),
                            edited_files,
                            overwrite_confirmation_needed: true,
                            transaction: None,
                            commit: None,
                        },
                        None,
                    ));
                }
                Err(refusal) => return Err(refusal.to_string()),
            };
            project_repositories
                .announce_the_transaction_if_applied_now(&project_repository_held, &outcome);
            let venv_to_make_current =
                VenvToMakeCurrent::once_applied_now(&project_repository_held, &outcome);
            let applied_now = outcome.was_applied_now();
            let applied = outcome.into_applied_transaction();
            let updated = the_import_the_transaction_placed(&applied)?;
            let importing_project_directory =
                project_repository_held.project_directory().to_path_buf();
            drop(project_repository_held);
            if applied_now {
                StreamGraphTransactionExecutor {
                    operations_on_the_loaded_streams: operations_on_the_loaded_streams.as_ref(),
                    project_repositories: &project_repositories,
                    async_runtime: &async_runtime,
                }
                .reconcile_each_loaded_stream_holding_a_stream_fragment(
                    &importing_project_directory,
                );
            }
            Ok((
                UpdateDependencyToolResult {
                    dependency: the_dependency_on_the_wire(&updated),
                    edited_files: files_the_update_overwrites,
                    overwrite_confirmation_needed: false,
                    transaction: Some(applied.transaction.transaction.to_string()),
                    commit: Some(applied.commit),
                },
                venv_to_make_current,
            ))
        })
        .await
        .map_err(|join_failure| {
            format!("update_dependency failed: the call did not finish: {join_failure}")
        })?
        .map_err(|refusal| format!("update_dependency failed: {refusal}"))?;
        let (answered, venv_to_make_current) = answered;
        self.make_the_venv_current_and_announce_it(venv_to_make_current);
        Ok(answered)
    }

    pub(crate) async fn list_dependencies(
        &self,
        ListDependenciesRequest {
            project_directory,
            check_for_newer,
        }: ListDependenciesRequest,
    ) -> Result<ListDependenciesToolResult, ControlOperationRefusal> {
        let project_repositories = Arc::clone(&self.project_repositories);
        let answered = tokio::task::spawn_blocking(move || {
            let project_repository =
                the_project_repository_open_at(&project_repositories, &project_directory)?;
            let listing = project_repository
                .lock()
                .import_listing(check_for_newer.unwrap_or(false));
            Ok::<_, String>(ListDependenciesToolResult {
                dependencies: listing
                    .into_iter()
                    .map(|listed| {
                        let mut check_refused_because = Vec::new();
                        let edited_files = listed.edited_files.unwrap_or_else(|why| {
                            check_refused_because.push(why);
                            Vec::new()
                        });
                        let version_on_the_source = match listed.version_on_the_source {
                            None => None,
                            Some(Ok(version)) => Some(version),
                            Some(Err(why)) => {
                                check_refused_because.push(why);
                                None
                            }
                        };
                        ListedProjectDependencyOnTheWire {
                            newer_version_exists: version_on_the_source
                                .as_ref()
                                .map(|version| *version != listed.import.version),
                            version_on_the_source,
                            edited_files,
                            check_refused_because: (!check_refused_because.is_empty())
                                .then(|| check_refused_because.join("; ")),
                            dependency: the_dependency_on_the_wire(&listed.import),
                        }
                    })
                    .collect(),
            })
        })
        .await
        .map_err(|join_failure| {
            format!("list_dependencies failed: the call did not finish: {join_failure}")
        })?
        .map_err(|refusal| format!("list_dependencies failed: {refusal}"))?;
        Ok(answered)
    }

    pub(crate) async fn remove_dependency(
        &self,
        RemoveDependencyRequest {
            project_directory,
            project,
            author,
            idempotency_key,
        }: RemoveDependencyRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<DependencyTransactionToolResult, ControlOperationRefusal> {
        let author = the_author_of_a_change(author, request_origin);
        let project = the_import_named(&project)
            .map_err(|refusal| format!("remove_dependency failed: {refusal}"))?;
        let project_repositories = Arc::clone(&self.project_repositories);
        let answered = tokio::task::spawn_blocking(move || {
            let project_repository =
                the_project_repository_open_at(&project_repositories, &project_directory)?;
            let mut project_repository_held = project_repository.lock();
            let idempotency_key = the_idempotency_key_a_caller_sent(idempotency_key)
                .map_err(|refusal| refusal.to_string())?;
            let outcome = project_repository_held
                .remove_dependency(&author, idempotency_key.as_ref(), project)
                .map_err(|refusal| refusal.to_string())?;
            project_repositories
                .announce_the_transaction_if_applied_now(&project_repository_held, &outcome);
            let venv_to_make_current =
                VenvToMakeCurrent::once_applied_now(&project_repository_held, &outcome);
            let applied = outcome.into_applied_transaction();
            let removed = the_import_the_transaction_removed(&applied)?;
            Ok::<_, String>((
                DependencyTransactionToolResult {
                    dependency: the_dependency_on_the_wire(&removed),
                    transaction: applied.transaction.transaction.to_string(),
                    commit: applied.commit,
                },
                venv_to_make_current,
            ))
        })
        .await
        .map_err(|join_failure| {
            format!("remove_dependency failed: the call did not finish: {join_failure}")
        })?
        .map_err(|refusal| format!("remove_dependency failed: {refusal}"))?;
        let (answered, venv_to_make_current) = answered;
        self.make_the_venv_current_and_announce_it(venv_to_make_current);
        Ok(answered)
    }

    pub(crate) async fn use_dependency_stream(
        &self,
        UseDependencyStreamRequest {
            stream,
            project,
            dependency_stream,
            name,
            author,
            idempotency_key,
        }: UseDependencyStreamRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<
        streamlib_runtime_client_contract::local_api_wire_contract::GraphTransactionToolResult,
        ControlOperationRefusal,
    > {
        let operation = StreamGraphAction::UseDependencyStream {
            stream,
            project,
            dependency_stream,
            name,
            label: None,
        };
        self.apply_a_graph_transaction(
            "use_dependency_stream",
            vec![operation],
            Some(GraphEditMode::Commit),
            author,
            idempotency_key,
            request_origin,
        )
        .await
    }
}

/// The project whose venv an import transaction applied now leaves behind what its
/// `pyproject.toml` declares.
struct VenvToMakeCurrent {
    project_directory: PathBuf,
    project: Uuid,
}

impl VenvToMakeCurrent {
    /// The venv of the project `project_repository_held` holds, when `outcome` applied now and
    /// the project declares its Python in a `pyproject.toml`.
    fn once_applied_now(
        project_repository_held: &ProjectRepository,
        outcome: &ProjectActionApplyOutcome,
    ) -> Option<Self> {
        let project_directory = project_repository_held.project_directory().to_path_buf();
        (outcome.was_applied_now() && project_directory.join(PYPROJECT_FILE_NAME).is_file()).then(
            || Self {
                project_directory,
                project: project_repository_held.project_file().project,
            },
        )
    }
}

impl ControlOperationDispatcher {
    /// Queue the making current of `venv_to_make_current`'s venv, off the answer, and announce
    /// it on its project's change feed once the action is done, so a client lists the node types
    /// the import's installed distributions declare.
    fn make_the_venv_current_and_announce_it(
        &self,
        venv_to_make_current: Option<VenvToMakeCurrent>,
    ) {
        let Some(VenvToMakeCurrent {
            project_directory,
            project,
        }) = venv_to_make_current
        else {
            return;
        };
        let mut stream_action_changes = self
            .operations_on_the_loaded_streams
            .listen_for_each_stream_action_change();
        let queued = match self
            .operations_on_the_loaded_streams
            .make_the_projects_venv_current(&project_directory)
        {
            Ok(queued) => queued,
            Err(not_queued) => {
                tracing::warn!(
                    project_directory = %project_directory.display(),
                    %not_queued,
                    "the venv was not made current after the project's imports changed"
                );
                return;
            }
        };
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        let project_repositories = Arc::clone(&self.project_repositories);
        tokio::spawn(async move {
            loop {
                match stream_action_changes.recv().await {
                    Ok(changed_action_id) if changed_action_id != queued.action_id => continue,
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return,
                }
                let Ok(action) =
                    operations_on_the_loaded_streams.describe_stream_action(&queued.action_id)
                else {
                    return;
                };
                match action.state {
                    StreamActionState::Done => {
                        project_repositories.announce_the_venv_made_current(project);
                        return;
                    }
                    StreamActionState::Failed | StreamActionState::Cancelled => {
                        tracing::warn!(
                            project_directory = %project_directory.display(),
                            failed_because = action.failed_because.as_deref().unwrap_or(""),
                            "the venv was not made current after the project's imports changed"
                        );
                        return;
                    }
                    StreamActionState::Queued | StreamActionState::InProgress => {}
                }
            }
        });
    }
}

fn the_import_named(project: &str) -> Result<Uuid, String> {
    Uuid::parse_str(project).map_err(|_| {
        format!("`{project}` is no project's UUID; `tatolab-project.json` lists each import's")
    })
}

fn the_project_repository_open_at(
    project_repositories: &ProjectRepositoriesOpenInThisRuntime,
    project_directory: &std::path::Path,
) -> Result<ProjectRepositoryOneWriterAtATime, String> {
    if !project_directory.is_absolute() {
        return Err(format!(
            "`project_directory` must be an absolute path; got `{}`",
            project_directory.display()
        ));
    }
    project_repositories
        .the_project_repository_at(project_directory)
        .map_err(|refusal| refusal.to_string())
}

fn the_import_the_transaction_placed(
    applied: &AppliedProjectTransaction,
) -> Result<ProjectImport, String> {
    applied
        .transaction
        .operations
        .iter()
        .find_map(|operation| match operation {
            ProjectOperation::Import(ProjectImportOperation::PlaceImport { import, .. }) => {
                Some(import.clone())
            }
            _ => None,
        })
        .ok_or_else(|| "the transaction recorded no import".to_owned())
}

fn the_import_the_transaction_removed(
    applied: &AppliedProjectTransaction,
) -> Result<ProjectImport, String> {
    applied
        .transaction
        .operations
        .iter()
        .find_map(|operation| match operation {
            ProjectOperation::Import(ProjectImportOperation::RemoveImport { import }) => {
                Some(import.clone())
            }
            _ => None,
        })
        .ok_or_else(|| "the transaction recorded no import it removed".to_owned())
}

fn the_dependency_on_the_wire(import: &ProjectImport) -> ProjectDependencyOnTheWire {
    ProjectDependencyOnTheWire {
        project: import.project.to_string(),
        folder: import.folder.clone(),
        source: import.source.clone(),
        version: import.version.clone(),
    }
}
