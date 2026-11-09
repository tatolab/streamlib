// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The dependency tools: another project pinned at a revision, its pin moved, its streams used
//! as stored fragments, and one of them forked into this project.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::schemars::JsonSchema;
use rmcp::{Peer, RoleServer, tool, tool_router};
use serde::Deserialize;
use serde_json::Value;
use streamlib_project_repository::{
    AppliedProjectTransaction, DependencyStreamFork, PinnedProjectDependency,
    ProjectDependencyOperation, ProjectOperation, ProjectRepositoriesOpenInThisRuntime,
    ProjectRepository,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    DependencyTransactionToolResult, ForkAssetToolResult, ForkedAssetOriginOnTheWire,
    GraphTransactionToolResult, ProjectDependencyOnTheWire,
};
use uuid::Uuid;

use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer, serialized_tool_result};
use crate::mcp_project_asset_tools::{
    AUTHOR_ARGUMENT_DESCRIPTION, ProjectTransactionAuthorToolArgument, project_asset_on_the_wire,
    the_author_of_a_change,
};
use crate::mcp_stream_graph_tools::{
    DEPENDENCY_PROJECT_ARGUMENT_DESCRIPTION, DEPENDENCY_STREAM_ARGUMENT_DESCRIPTION,
    DEPENDENCY_STREAM_NAME_ARGUMENT_DESCRIPTION, GRAPH_EDIT_STREAM_ARGUMENT_DESCRIPTION,
    GraphEditMode, GraphTransactionInFlight, GraphTransactionStep, STREAM_FRAGMENT_CONFIG_KEY,
    STREAM_FRAGMENT_TYPE, StreamGraphOperationToolArgument,
};

const DEPENDING_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION: &str =
    "The project that depends, by absolute path: a folder holding `tatolab-project.json`.";

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct AddDependencyToolArguments {
    #[schemars(description = DEPENDING_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(
        description = "Where the project depended on is fetched from: its project directory by absolute path, or the URL of a Git remote holding its history."
    )]
    source: String,
    #[schemars(
        description = "The commit, tag or branch of its history to pin to; the pin records the commit it names now."
    )]
    revision: String,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct UpdateDependencyToolArguments {
    #[schemars(description = DEPENDING_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(description = DEPENDENCY_PROJECT_ARGUMENT_DESCRIPTION)]
    project: String,
    #[schemars(
        description = "The commit, tag or branch of its history to move the pin to, fetched from its source first."
    )]
    revision: String,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
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
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ForkAssetToolArguments {
    #[schemars(description = DEPENDING_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(description = DEPENDENCY_PROJECT_ARGUMENT_DESCRIPTION)]
    project: String,
    #[schemars(
        description = "The depended-on project's asset to copy, by its UUID; a stream, by the `asset` its graph document holds."
    )]
    asset: String,
    #[schemars(
        description = "Where the copy's graph document is written, relative to the project; `streams/<name>-fork.graph.json` when omitted."
    )]
    path: Option<String>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
}

#[tool_router(router = project_dependency_tool_router, vis = "pub(crate)")]
impl LocalApiMcpServerHandler {
    #[tool(
        description = "Depend on another project, pinned at a revision, as one transaction by `author`: its history is fetched from `source` into `.tatolab/dependencies/<project uuid>/` and the pin, the commit `revision` names, is recorded in `tatolab-project.json`. A local project directory needs no network; a Git remote needs it only to fetch. A clone of the project carries the pin without the fetched history, which is fetched again from the recorded `source` the first time it is read, the pin left where it is. Its assets are then referenced as `{\"asset\": \"<uuid>\", \"from\": \"<project uuid>\"}` and read at the pin; its streams are placed with `use_dependency_stream`. Answers the `dependency` with its pinned `revision`. Refused, nothing pinned, for a revision the source does not hold, a source holding no project, the project itself, or a project already depended on."
    )]
    async fn add_dependency(
        &self,
        peer: Peer<RoleServer>,
        Parameters(AddDependencyToolArguments {
            project_directory,
            source,
            revision,
            author,
        }): Parameters<AddDependencyToolArguments>,
    ) -> ToolCallAnswer {
        let author = the_author_of_a_change(author, &peer);
        let project_repositories = Arc::clone(&self.project_repositories_open_in_this_runtime);
        let answered = tokio::task::spawn_blocking(move || {
            let project_repository =
                the_project_repository_open_at(&project_repositories, &project_directory)?;
            let mut project_repository_held = project_repository.lock();
            let applied = project_repository_held
                .add_dependency(&author, &source, &revision)
                .map_err(|refusal| refusal.to_string())?;
            let added = applied
                .transaction
                .operations
                .iter()
                .find_map(|operation| match operation {
                    ProjectOperation::Dependency(ProjectDependencyOperation::AddDependency {
                        dependency,
                    }) => Some(dependency.clone()),
                    _ => None,
                })
                .ok_or("the transaction recorded no dependency")?;
            project_repositories
                .announce_applied_transaction(&project_repository_held, applied.clone());
            Ok::<_, String>(the_answer_of_a_dependency_transaction(
                &added,
                TransactionAsHistoryRecordsIt::of(&applied),
                Vec::new(),
            ))
        })
        .await
        .map_err(|join_failure| {
            format!("add_dependency failed: the call did not finish: {join_failure}")
        })?
        .map_err(|refusal| format!("add_dependency failed: {refusal}"))?;
        serialized_tool_result("add_dependency", &answered)
    }

    #[tool(
        description = "Move a dependency's pin to another revision, as one transaction by `author`: its history is fetched again from its source, and every fragment of this project's loaded streams that holds one of its streams is refreshed from the new pin, each node keeping its UUID by name so links into the fragment and edits around it hold. Answers the `dependency` and the `graph_operations` that refreshed each fragment; undo moves the pin back and restores each fragment as it was. Refused, nothing changed, for a project not depended on or a revision its source does not hold."
    )]
    async fn update_dependency(
        &self,
        peer: Peer<RoleServer>,
        Parameters(UpdateDependencyToolArguments {
            project_directory,
            project,
            revision,
            author,
        }): Parameters<UpdateDependencyToolArguments>,
    ) -> ToolCallAnswer {
        let author = the_author_of_a_change(author, &peer);
        let project = the_dependency_named(&project)
            .map_err(|refusal| format!("update_dependency failed: {refusal}"))?;
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        let project_repositories = Arc::clone(&self.project_repositories_open_in_this_runtime);
        let async_runtime = tokio::runtime::Handle::current();
        let answered = tokio::task::spawn_blocking(move || {
            let project_repository =
                the_project_repository_open_at(&project_repositories, &project_directory)?;
            let graph_transaction = GraphTransactionInFlight {
                operations_on_the_loaded_streams: operations_on_the_loaded_streams.as_ref(),
                project_repositories: &project_repositories,
                async_runtime: &async_runtime,
            };
            let applied = graph_transaction.apply_what_the_held_project_prepares(
                &project_directory,
                &author,
                |project_repository_held, stream_id_of_each_stream_asset| {
                    let pin_move = project_repository_held
                        .the_pin_move_a_dependency_update_makes(project, &revision)
                        .map_err(|refusal| refusal.to_string())?;
                    let refreshes = the_refresh_of_each_fragment_holding_a_stream_of(
                        project_repository_held,
                        stream_id_of_each_stream_asset,
                        project,
                    )?;
                    Ok((refreshes, vec![ProjectOperation::Dependency(pin_move)]))
                },
            )?;
            let dependency = project_repository
                .lock()
                .dependency(project)
                .map_err(|refusal| refusal.to_string())?;
            let (transaction, graph_operations) =
                TransactionAsHistoryRecordsIt::of_the_answer(applied)?;
            Ok::<_, String>(the_answer_of_a_dependency_transaction(
                &dependency,
                transaction,
                graph_operations,
            ))
        })
        .await
        .map_err(|join_failure| {
            format!("update_dependency failed: the call did not finish: {join_failure}")
        })?
        .map_err(|refusal| format!("update_dependency failed: {refusal}"))?;
        serialized_tool_result("update_dependency", &answered)
    }

    #[tool(
        description = "Place a stream of a project this one depends on in a loaded stream's graph, as one committed transaction: a fragment node of type `tatolab.project:StreamFragment` whose config `{\"stream\": {\"asset\", \"from\"}}` names the stream and holds a stored copy of its nodes and links at the pin, each with a UUID of its own. Its Graph Input and Output nodes are the fragment's ports. Used again under the name of a fragment holding the same stream, it refreshes that fragment from the pin. `update_dependency` refreshes it when the pin moves; `fork_asset` makes it this project's own. Answers as `transact` does."
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
            &peer,
        )
        .await
    }

    #[tool(
        description = "Fork a stream of a project this one depends on into this project, as one transaction by `author`: its graph document at the pin is copied to `path` under a new stream asset UUID, recording the stream, project and commit it came from under `forked_from`, and every node of this project that referenced it as `{\"asset\", \"from\"}`, in its config or at any level of a stored fragment, is made to reference the copy, so a later `update_dependency` no longer reaches it. The copy is this project's own, edited like any of its streams. Answers the copy as an asset, `forked_from`, and the `graph_operations` applied to loaded streams; undo points each reference back, takes the copy out and unloads its stream, refused once the copy has been edited. Refused for an asset the dependency's history holds no graph document of, since only a stream is forked, and for a referencing node of a graph document that holds no UUID yet."
    )]
    async fn fork_asset(
        &self,
        peer: Peer<RoleServer>,
        Parameters(ForkAssetToolArguments {
            project_directory,
            project,
            asset,
            path,
            author,
        }): Parameters<ForkAssetToolArguments>,
    ) -> ToolCallAnswer {
        let author = the_author_of_a_change(author, &peer);
        let project = the_dependency_named(&project)
            .map_err(|refusal| format!("fork_asset failed: {refusal}"))?;
        let asset = Uuid::parse_str(&asset)
            .map_err(|_| format!("fork_asset failed: `{asset}` is no asset's UUID"))?;
        let operations_on_the_loaded_streams = Arc::clone(&self.operations_on_the_loaded_streams);
        let project_repositories = Arc::clone(&self.project_repositories_open_in_this_runtime);
        let async_runtime = tokio::runtime::Handle::current();
        let answered = tokio::task::spawn_blocking(move || {
            let project_repository =
                the_project_repository_open_at(&project_repositories, &project_directory)?;
            let graph_transaction = GraphTransactionInFlight {
                operations_on_the_loaded_streams: operations_on_the_loaded_streams.as_ref(),
                project_repositories: &project_repositories,
                async_runtime: &async_runtime,
            };
            let mut copy_and_the_pin_it_was_forked_at = None;
            let applied = graph_transaction.apply_what_the_held_project_prepares(
                &project_directory,
                &author,
                |project_repository_held, stream_id_of_each_stream_asset| {
                    let fork = project_repository_held
                        .the_fork_of_a_dependency_stream(project, asset, path.as_deref())
                        .map_err(|refusal| refusal.to_string())?;
                    let forked_at = project_repository_held
                        .dependency(project)
                        .map_err(|refusal| refusal.to_string())?
                        .revision;
                    copy_and_the_pin_it_was_forked_at = Some((fork.copy, forked_at));
                    Ok(the_fork_split_by_loaded_stream(
                        &fork,
                        stream_id_of_each_stream_asset,
                    ))
                },
            )?;
            let (copy, forked_at) =
                copy_and_the_pin_it_was_forked_at.ok_or("the fork was applied without its copy")?;
            let copy = project_repository
                .lock()
                .asset(copy)
                .map_err(|refusal| refusal.to_string())?;
            let (transaction, graph_operations) =
                TransactionAsHistoryRecordsIt::of_the_answer(applied)?;
            Ok::<_, String>(ForkAssetToolResult {
                asset: project_asset_on_the_wire(&copy),
                forked_from: ForkedAssetOriginOnTheWire {
                    asset: asset.to_string(),
                    from: project.to_string(),
                    revision: forked_at,
                },
                transaction: transaction.transaction,
                commit: transaction.commit,
                graph_operations,
            })
        })
        .await
        .map_err(|join_failure| {
            format!("fork_asset failed: the call did not finish: {join_failure}")
        })?
        .map_err(|refusal| format!("fork_asset failed: {refusal}"))?;
        serialized_tool_result("fork_asset", &answered)
    }
}

fn the_dependency_named(project: &str) -> Result<Uuid, String> {
    Uuid::parse_str(project).map_err(|_| {
        format!("`{project}` is no project's UUID; `tatolab-project.json` lists each dependency's")
    })
}

fn the_project_repository_open_at(
    project_repositories: &ProjectRepositoriesOpenInThisRuntime,
    project_directory: &std::path::Path,
) -> Result<streamlib_project_repository::ProjectRepositoryOneWriterAtATime, String> {
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

/// A committed transaction as a tool answers it: its UUID and the commit recording it.
struct TransactionAsHistoryRecordsIt {
    transaction: String,
    commit: String,
}

impl TransactionAsHistoryRecordsIt {
    fn of(applied: &AppliedProjectTransaction) -> Self {
        Self {
            transaction: applied.transaction.transaction.to_string(),
            commit: applied.commit.clone(),
        }
    }

    /// The committed transaction a graph transaction answers, and each graph operation it
    /// applied to a loaded stream as `transact` answers it.
    fn of_the_answer(answered: GraphTransactionToolResult) -> Result<(Self, Vec<Value>), String> {
        let (Some(transaction), Some(commit)) = (answered.transaction, answered.commit) else {
            return Err("the committed transaction answered no commit".to_owned());
        };
        Ok((
            Self {
                transaction,
                commit,
            },
            answered.operations,
        ))
    }
}

fn the_answer_of_a_dependency_transaction(
    dependency: &PinnedProjectDependency,
    applied: TransactionAsHistoryRecordsIt,
    graph_operations: Vec<Value>,
) -> DependencyTransactionToolResult {
    DependencyTransactionToolResult {
        dependency: ProjectDependencyOnTheWire {
            project: dependency.project.to_string(),
            source: dependency.source.clone(),
            revision: dependency.revision.clone(),
        },
        transaction: applied.transaction,
        commit: applied.commit,
        graph_operations,
    }
}

/// The `use_dependency_stream` that refreshes each top-level fragment of this project's loaded
/// streams, `stream_id_of_each_stream_asset`, holding a stream of the dependency `project`.
fn the_refresh_of_each_fragment_holding_a_stream_of(
    project_repository: &ProjectRepository,
    stream_id_of_each_stream_asset: &HashMap<Uuid, String>,
    project: Uuid,
) -> Result<Vec<GraphTransactionStep>, String> {
    let mut refreshes = Vec::new();
    for (stream_asset, stream_id) in stream_id_of_each_stream_asset {
        let graph_document = project_repository
            .stream_graph_document(*stream_asset)
            .map_err(|refusal| refusal.to_string())?;
        for fragment_node in graph_document.fragment_nodes() {
            if fragment_node.node_type != STREAM_FRAGMENT_TYPE {
                continue;
            }
            let held_stream = &fragment_node.config[STREAM_FRAGMENT_CONFIG_KEY];
            if held_stream["from"].as_str() != Some(project.to_string().as_str()) {
                continue;
            }
            let Some(dependency_stream) = held_stream["asset"].as_str() else {
                continue;
            };
            refreshes.push(GraphTransactionStep::Requested(
                StreamGraphOperationToolArgument::UseDependencyStream {
                    stream: stream_id.clone(),
                    project: project.to_string(),
                    dependency_stream: dependency_stream.to_owned(),
                    name: Some(fragment_node.name),
                    label: None,
                },
            ));
        }
    }
    Ok(refreshes)
}

/// `fork` as the operation pointing each reference on a loaded stream at the copy, and the
/// operations applied before them: the copy, then each reference on a stream not loaded.
fn the_fork_split_by_loaded_stream(
    fork: &DependencyStreamFork,
    stream_id_of_each_stream_asset: &HashMap<Uuid, String>,
) -> (Vec<GraphTransactionStep>, Vec<ProjectOperation>) {
    let mut references_on_loaded_streams = Vec::new();
    let mut project_operations_first = vec![ProjectOperation::StreamDocument(
        fork.create_the_copy.clone(),
    )];
    for reference in &fork.references_made_local {
        match stream_id_of_each_stream_asset.get(&reference.stream) {
            Some(stream_id) => references_on_loaded_streams.push(GraphTransactionStep::Prepared {
                stream: stream_id.clone(),
                graph_operation: reference.graph_operation.clone(),
            }),
            None => project_operations_first.push(ProjectOperation::StreamGraph(reference.clone())),
        }
    }
    (references_on_loaded_streams, project_operations_first)
}
