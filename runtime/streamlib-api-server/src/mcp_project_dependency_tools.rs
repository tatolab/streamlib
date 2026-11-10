// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The dependency tools: another project copied whole into this one's `imports/` folder, copied
//! again, listed and taken out, and its streams placed as fragments referencing them.

use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::schemars::JsonSchema;
use rmcp::{Peer, RoleServer, tool, tool_router};
use serde::Deserialize;
use streamlib_project_repository::{
    AppliedProjectTransaction, ProjectImport, ProjectRepositoriesOpenInThisRuntime,
    ProjectRepositoryOneWriterAtATime,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    DependencyTransactionToolResult, ListDependenciesToolResult, ListedProjectDependencyOnTheWire,
    ProjectDependencyOnTheWire, UpdateDependencyToolResult,
};
use uuid::Uuid;

use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer, serialized_tool_result};
use crate::mcp_project_asset_tools::{
    AUTHOR_ARGUMENT_DESCRIPTION, IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION,
    ProjectTransactionAuthorToolArgument, the_author_of_a_change,
    the_idempotency_key_a_caller_sent,
};
use crate::mcp_stream_graph_tools::{
    DEPENDENCY_PROJECT_ARGUMENT_DESCRIPTION, DEPENDENCY_STREAM_ARGUMENT_DESCRIPTION,
    DEPENDENCY_STREAM_NAME_ARGUMENT_DESCRIPTION, GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION,
    GraphEditMode, GraphTransactionInFlight, StreamGraphOperationToolArgument,
};

const IMPORTING_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION: &str =
    "The project that imports, by absolute path: a folder holding `tatolab-project.json`.";

const IMPORT_SOURCE_ARGUMENT_DESCRIPTION: &str = "Where the project is copied from, on this machine, by absolute path: its project folder, or a `.zip` of one, such as a GitHub archive downloaded first. It must hold a `tatolab-project.json` with a project UUID, at its root or in its one top folder.";

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct AddDependencyToolArguments {
    #[schemars(description = IMPORTING_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(description = IMPORT_SOURCE_ARGUMENT_DESCRIPTION)]
    source: String,
    #[schemars(
        description = "The folder under `imports/` its copy takes; the imported project's name, lowercase with each run of other characters made `-`, when omitted."
    )]
    folder: Option<String>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct UpdateDependencyToolArguments {
    #[schemars(description = IMPORTING_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(description = DEPENDENCY_PROJECT_ARGUMENT_DESCRIPTION)]
    project: String,
    #[schemars(
        description = "Where to copy it from this time, as `add_dependency` takes it; the `source` it was last copied from when omitted."
    )]
    source: Option<String>,
    #[schemars(
        description = "Overwrite the files of its folder edited since its copy, which an update without it lists under `edited_files` and leaves as they are."
    )]
    overwrite_edited_files: Option<bool>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ListDependenciesToolArguments {
    #[schemars(description = IMPORTING_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(
        description = "Read each import's `source` and answer the version it holds now beside the copy's."
    )]
    check_for_newer: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct RemoveDependencyToolArguments {
    #[schemars(description = IMPORTING_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(description = DEPENDENCY_PROJECT_ARGUMENT_DESCRIPTION)]
    project: String,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct UseDependencyStreamToolArguments {
    #[schemars(description = GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION)]
    stream: String,
    #[schemars(description = DEPENDENCY_PROJECT_ARGUMENT_DESCRIPTION)]
    project: String,
    #[schemars(description = DEPENDENCY_STREAM_ARGUMENT_DESCRIPTION)]
    dependency_stream: String,
    #[schemars(description = DEPENDENCY_STREAM_NAME_ARGUMENT_DESCRIPTION)]
    name: Option<String>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[tool_router(router = project_dependency_tool_router, vis = "pub(crate)")]
impl LocalApiMcpServerHandler {
    #[tool(
        description = "Import another project, as one transaction by `author`: the project `source` holds, a folder or a `.zip` of one on this machine, is copied whole into `imports/<folder>/` and recorded in `tatolab-project.json` under `dependencies` with the `version` its copy carries, the SHA-256 over its files. Nothing reaches the network. The copy is this project's own from then on: its assets are referenced as `{\"asset\": \"<uuid>\", \"from\": \"<project uuid>\"}` and read from its folder, its streams are placed with `use_dependency_stream`, and to edit it, open `imports/<folder>/` as a project of its own. Answers the `dependency`. Refused, nothing copied, for a source holding no `tatolab-project.json` or no project UUID in it, naming what is missing, for this project itself, a project already imported, or a folder already held."
    )]
    async fn add_dependency(
        &self,
        peer: Peer<RoleServer>,
        Parameters(AddDependencyToolArguments {
            project_directory,
            source,
            folder,
            author,
            idempotency_key,
        }): Parameters<AddDependencyToolArguments>,
    ) -> ToolCallAnswer {
        let author = the_author_of_a_change(author, &peer);
        let project_repositories = Arc::clone(&self.project_repositories_open_in_this_runtime);
        let answered = tokio::task::spawn_blocking(move || {
            let project_repository =
                the_project_repository_open_at(&project_repositories, &project_directory)?;
            let mut project_repository_held = project_repository.lock();
            let idempotency_key = the_idempotency_key_a_caller_sent(idempotency_key)
                .map_err(|refusal| refusal.to_string())?;
            let applied = match project_repository_held
                .the_transaction_applied_under_a_key_the_caller_sent(idempotency_key.as_ref())
                .map_err(|refusal| refusal.to_string())?
            {
                Some(applied_before) => applied_before,
                None => {
                    let applied = project_repository_held
                        .add_dependency(
                            &author,
                            idempotency_key.as_ref(),
                            &source,
                            folder.as_deref(),
                        )
                        .map_err(|refusal| refusal.to_string())?;
                    project_repositories
                        .announce_applied_transaction(&project_repository_held, applied.clone());
                    applied
                }
            };
            let added = the_import_the_transaction_placed(&applied)?;
            Ok::<_, String>(DependencyTransactionToolResult {
                dependency: the_dependency_on_the_wire(&added),
                transaction: applied.transaction.transaction.to_string(),
                commit: applied.commit,
            })
        })
        .await
        .map_err(|join_failure| {
            format!("add_dependency failed: the call did not finish: {join_failure}")
        })?
        .map_err(|refusal| format!("add_dependency failed: {refusal}"))?;
        serialized_tool_result("add_dependency", &answered)
    }

    #[tool(
        description = "Copy an import again, whole, as one transaction by `author`, from `source` or from the source it was last copied from, replacing its folder under `imports/` and its `version`. Every loaded stream holding one of its streams as a fragment follows the new copy. When files of its folder were edited since its copy, nothing changes: it answers them under `edited_files` with `overwrite_confirmation_needed`, and is sent again with `overwrite_edited_files` to overwrite them. Undo puts the folder back as it was. Refused, nothing changed, for a project not imported, or a source holding another project."
    )]
    async fn update_dependency(
        &self,
        peer: Peer<RoleServer>,
        Parameters(UpdateDependencyToolArguments {
            project_directory,
            project,
            source,
            overwrite_edited_files,
            author,
            idempotency_key,
        }): Parameters<UpdateDependencyToolArguments>,
    ) -> ToolCallAnswer {
        let author = the_author_of_a_change(author, &peer);
        let project = the_import_named(&project)
            .map_err(|refusal| format!("update_dependency failed: {refusal}"))?;
        let overwrite_edited_files = overwrite_edited_files.unwrap_or(false);
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        let project_repositories = Arc::clone(&self.project_repositories_open_in_this_runtime);
        let async_runtime = tokio::runtime::Handle::current();
        let answered = tokio::task::spawn_blocking(move || {
            let project_repository =
                the_project_repository_open_at(&project_repositories, &project_directory)?;
            let mut project_repository_held = project_repository.lock();
            let idempotency_key = the_idempotency_key_a_caller_sent(idempotency_key)
                .map_err(|refusal| refusal.to_string())?;
            if let Some(applied_before) = project_repository_held
                .the_transaction_applied_under_a_key_the_caller_sent(idempotency_key.as_ref())
                .map_err(|refusal| refusal.to_string())?
            {
                return Ok(UpdateDependencyToolResult {
                    dependency: the_dependency_on_the_wire(&the_import_the_transaction_placed(
                        &applied_before,
                    )?),
                    edited_files: Vec::new(),
                    overwrite_confirmation_needed: false,
                    transaction: Some(applied_before.transaction.transaction.to_string()),
                    commit: Some(applied_before.commit),
                });
            }
            let held = project_repository_held
                .import(project)
                .map_err(|refusal| refusal.to_string())?;
            let edited_files = project_repository_held
                .files_edited_in_the_import(&held)
                .map_err(|refusal| refusal.to_string())?;
            if !edited_files.is_empty() && !overwrite_edited_files {
                return Ok::<_, String>(UpdateDependencyToolResult {
                    dependency: the_dependency_on_the_wire(&held),
                    edited_files,
                    overwrite_confirmation_needed: true,
                    transaction: None,
                    commit: None,
                });
            }
            let applied = project_repository_held
                .update_dependency(
                    &author,
                    idempotency_key.as_ref(),
                    project,
                    source.as_deref(),
                    overwrite_edited_files,
                )
                .map_err(|refusal| refusal.to_string())?;
            let updated = the_import_the_transaction_placed(&applied)?;
            let importing_project_directory =
                project_repository_held.project_directory().to_path_buf();
            project_repositories
                .announce_applied_transaction(&project_repository_held, applied.clone());
            drop(project_repository_held);
            GraphTransactionInFlight {
                operations_on_the_loaded_streams: operations_on_the_loaded_streams.as_ref(),
                project_repositories: &project_repositories,
                async_runtime: &async_runtime,
            }
            .reconcile_each_loaded_stream_holding_a_stream_fragment(&importing_project_directory);
            Ok(UpdateDependencyToolResult {
                dependency: the_dependency_on_the_wire(&updated),
                edited_files,
                overwrite_confirmation_needed: false,
                transaction: Some(applied.transaction.transaction.to_string()),
                commit: Some(applied.commit),
            })
        })
        .await
        .map_err(|join_failure| {
            format!("update_dependency failed: the call did not finish: {join_failure}")
        })?
        .map_err(|refusal| format!("update_dependency failed: {refusal}"))?;
        serialized_tool_result("update_dependency", &answered)
    }

    #[tool(
        description = "List the projects this one imports, each with its `folder`, `source` and `version` as `tatolab-project.json` holds them, and the files of its folder edited since its copy under `edited_files`; changes nothing. With `check_for_newer`, each source is read again and each import also answers `version_on_the_source` and `newer_version_exists`, or, for a source that could not be read, `check_refused_because`; nothing is copied until `update_dependency` copies it."
    )]
    async fn list_dependencies(
        &self,
        Parameters(ListDependenciesToolArguments {
            project_directory,
            check_for_newer,
        }): Parameters<ListDependenciesToolArguments>,
    ) -> ToolCallAnswer {
        let project_repositories = Arc::clone(&self.project_repositories_open_in_this_runtime);
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
        serialized_tool_result("list_dependencies", &answered)
    }

    #[tool(
        description = "Stop importing a project, as one transaction by `author`: its folder leaves `imports/` and its entry `tatolab-project.json`; undo puts both back as they were. Answers the `dependency` as it was. Refused, nothing changed, while a node of any of this project's streams — a fragment holding one of its streams, or a config holding `{\"asset\", \"from\"}` naming it, at any depth — still references it, naming each such node. Refused for a project not imported."
    )]
    async fn remove_dependency(
        &self,
        peer: Peer<RoleServer>,
        Parameters(RemoveDependencyToolArguments {
            project_directory,
            project,
            author,
            idempotency_key,
        }): Parameters<RemoveDependencyToolArguments>,
    ) -> ToolCallAnswer {
        let author = the_author_of_a_change(author, &peer);
        let project = the_import_named(&project)
            .map_err(|refusal| format!("remove_dependency failed: {refusal}"))?;
        let project_repositories = Arc::clone(&self.project_repositories_open_in_this_runtime);
        let answered = tokio::task::spawn_blocking(move || {
            let project_repository =
                the_project_repository_open_at(&project_repositories, &project_directory)?;
            let mut project_repository_held = project_repository.lock();
            let idempotency_key = the_idempotency_key_a_caller_sent(idempotency_key)
                .map_err(|refusal| refusal.to_string())?;
            let (removed, applied) = match project_repository_held
                .the_transaction_applied_under_a_key_the_caller_sent(idempotency_key.as_ref())
                .map_err(|refusal| refusal.to_string())?
            {
                Some(applied_before) => (
                    the_import_the_transaction_removed(&applied_before)?,
                    applied_before,
                ),
                None => {
                    let removed = project_repository_held
                        .import(project)
                        .map_err(|refusal| refusal.to_string())?;
                    let applied = project_repository_held
                        .remove_dependency(&author, idempotency_key.as_ref(), project)
                        .map_err(|refusal| refusal.to_string())?;
                    project_repositories
                        .announce_applied_transaction(&project_repository_held, applied.clone());
                    (removed, applied)
                }
            };
            Ok::<_, String>(DependencyTransactionToolResult {
                dependency: the_dependency_on_the_wire(&removed),
                transaction: applied.transaction.transaction.to_string(),
                commit: applied.commit,
            })
        })
        .await
        .map_err(|join_failure| {
            format!("remove_dependency failed: the call did not finish: {join_failure}")
        })?
        .map_err(|refusal| format!("remove_dependency failed: {refusal}"))?;
        serialized_tool_result("remove_dependency", &answered)
    }

    #[tool(
        description = "Place a stream of an imported project in a loaded stream's graph, as one committed transaction: a fragment node of type `tatolab.project:StreamFragment` whose config `{\"stream\": {\"asset\", \"from\"}}` references the stream. The graph document stores the reference alone; each load, and each change to the stream or its import, places the stream's nodes and links as its graph document under `imports/<folder>/` is on disk, each node under the UUID UUIDv5(fragment node's UUID, its own). Its Graph Input and Output nodes are the fragment's ports. Answers as `transact` does. Refused for a name a node of the graph already holds, and for a stream that holds the stream it is placed in, naming each stream of the cycle."
    )]
    async fn use_dependency_stream(
        &self,
        peer: Peer<RoleServer>,
        Parameters(UseDependencyStreamToolArguments {
            stream,
            project,
            dependency_stream,
            name,
            author,
            idempotency_key,
        }): Parameters<UseDependencyStreamToolArguments>,
    ) -> ToolCallAnswer {
        let operation = StreamGraphOperationToolArgument::UseDependencyStream {
            stream,
            project,
            dependency_stream,
            name,
            label: None,
        };
        self.one_graph_transaction(
            "use_dependency_stream",
            vec![operation],
            Some(GraphEditMode::Commit),
            author,
            idempotency_key,
            &peer,
        )
        .await
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
            streamlib_project_repository::ProjectOperation::Import(
                streamlib_project_repository::ProjectImportOperation::PlaceImport {
                    import, ..
                },
            ) => Some(import.clone()),
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
            streamlib_project_repository::ProjectOperation::Import(
                streamlib_project_repository::ProjectImportOperation::RemoveImport { import },
            ) => Some(import.clone()),
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
