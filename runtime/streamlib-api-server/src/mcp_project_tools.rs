// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The project tools: a project renamed, described and given a cover, each one transaction on
//! its history, and duplicated or deleted whole.

use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::schemars::JsonSchema;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};
use serde::Deserialize;
use streamlib_project_repository::{
    AppliedProjectTransaction, ProjectOperation, ProjectRepository, ProjectRepositoryRefusal,
    ProjectSettingsOperation, ProjectTransactionAuthor, ProjectTransactionLineage,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    DeleteProjectToolResult, DuplicateProjectToolResult, ProjectSettingsTransactionToolResult,
};
use uuid::Uuid;

use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer, serialized_tool_result};
use crate::mcp_project_asset_tools::{
    ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION, AUTHOR_ARGUMENT_DESCRIPTION,
    IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION, ProjectTransactionAuthorToolArgument,
    the_author_of_a_change, the_idempotency_key_a_caller_sent,
};

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct RenameProjectToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(description = "The project's new name; never blank.")]
    name: String,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SetProjectDescriptionToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(description = "What the project is; `null` or blank clears it.")]
    description: Option<String>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SetProjectCoverToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(
        description = "The media asset the project is shown by, by the UUID `import_asset` or `list_assets` answered; `null` clears it."
    )]
    cover: Option<String>,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
    #[schemars(description = IDEMPOTENCY_KEY_ARGUMENT_DESCRIPTION)]
    idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct DuplicateProjectToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
    #[schemars(
        description = "Where the duplicate goes, by absolute path: a folder that does not exist yet, outside the project."
    )]
    into: PathBuf,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct DeleteProjectToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
}

/// The one change a project-settings tool asks of the project as it is now.
type ProjectSettingsChangeAskedOfTheProject =
    Box<dyn FnOnce(&ProjectRepository) -> ProjectSettingsOperation + Send>;

#[tool_router(router = project_tool_router, vis = "pub(crate)")]
impl LocalApiMcpServerHandler {
    #[tool(
        description = "Rename a project, as one transaction by `author` on its history that undoes like any edit: its `name` in `tatolab-project.json` changes, and its UUID, its folder and its streams' ids stay as they are. Answers how people see the project now, with the `transaction` and `commit`. Refused for a blank name."
    )]
    async fn rename_project(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(RenameProjectToolArguments {
            project_directory,
            name,
            author,
            idempotency_key,
        }): Parameters<RenameProjectToolArguments>,
    ) -> ToolCallAnswer {
        self.one_project_settings_change_as_a_transaction(
            "rename_project",
            project_directory,
            the_author_of_a_change(author, &tool_call_request_context),
            idempotency_key,
            Box::new(
                move |project_repository| ProjectSettingsOperation::SetProjectName {
                    from: project_repository.project_file().name.clone(),
                    to: name,
                },
            ),
        )
        .await
    }

    #[tool(
        description = "Describe a project, or clear its description, as one transaction by `author` on its history that undoes like any edit: its `description` in `tatolab-project.json`, served by `list_assets` as `project_description`. Answers how people see the project now, with the `transaction` and `commit`."
    )]
    async fn set_project_description(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(SetProjectDescriptionToolArguments {
            project_directory,
            description,
            author,
            idempotency_key,
        }): Parameters<SetProjectDescriptionToolArguments>,
    ) -> ToolCallAnswer {
        let description = description.filter(|description| !description.trim().is_empty());
        self.one_project_settings_change_as_a_transaction(
            "set_project_description",
            project_directory,
            the_author_of_a_change(author, &tool_call_request_context),
            idempotency_key,
            Box::new(
                move |project_repository| ProjectSettingsOperation::SetProjectDescription {
                    from: project_repository.project_file().description.clone(),
                    to: description,
                },
            ),
        )
        .await
    }

    #[tool(
        description = "Give a project a cover, or take it away, as one transaction by `author` on its history that undoes like any edit: its `cover` in `tatolab-project.json` names a media asset the project holds by its UUID, served by `list_assets` as `project_cover`. An image a person picks, or a frame a viewer showed, is first imported with `import_asset` and then named here; the asset cannot be deleted while it is the cover. Answers how people see the project now, with the `transaction` and `commit`. Refused for an asset the project does not hold, or one that is not a media asset."
    )]
    async fn set_project_cover(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(SetProjectCoverToolArguments {
            project_directory,
            cover,
            author,
            idempotency_key,
        }): Parameters<SetProjectCoverToolArguments>,
    ) -> ToolCallAnswer {
        let cover = cover
            .map(|cover| {
                Uuid::parse_str(&cover).map_err(|_| {
                    format!(
                        "set_project_cover failed: `cover` must be an asset's UUID; got `{cover}`"
                    )
                })
            })
            .transpose()?;
        self.one_project_settings_change_as_a_transaction(
            "set_project_cover",
            project_directory,
            the_author_of_a_change(author, &tool_call_request_context),
            idempotency_key,
            Box::new(
                move |project_repository| ProjectSettingsOperation::SetProjectCover {
                    from: project_repository.project_file().cover,
                    to: cover,
                },
            ),
        )
        .await
    }

    #[tool(
        description = "Duplicate a project into `into`, a folder that does not exist yet, as a project of its own: every file a copy of it carries, its `imports/` included, and its whole history, so its edits can still be undone in the copy, under a new project UUID and the name `<name> copy`, the duplicate recorded as a commit on its history by `author`. Its `.venv` and the rest of its `.tatolab/` stay behind, and the duplicate is not run: `run_project` runs it. Answers the duplicate's `project_directory`, `project`, `project_name` and `commit`. Refused, nothing left at `into`, for a relative `into`, one that exists, and one inside the project."
    )]
    async fn duplicate_project(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(DuplicateProjectToolArguments {
            project_directory,
            into,
            author,
        }): Parameters<DuplicateProjectToolArguments>,
    ) -> ToolCallAnswer {
        let author = the_author_of_a_change(author, &tool_call_request_context);
        let duplicated = self
            .a_project_repository_call_off_the_async_runtime(
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
            .await?;
        serialized_tool_result("duplicate_project", &duplicated)
    }

    #[tool(
        description = "Delete the whole project in `project_directory`: queue its deletion on its stream action queue and answer at once with the action's id as `action` beside the `project_directory`. Once the project's earlier actions have run, it unloads every loaded stream of the project and forgets every record of it, as `remove_project` does, then moves the project's folder to this machine's Trash, from which its owner can put it back. Its streams are `stopping` in `list_streams` until it ends; follow it with `describe_stream_action`, which answers `failed` with why when the folder could not be moved. Refused, with nothing queued: a `project_directory` that is not absolute, and a folder holding no `tatolab-project.json`."
    )]
    async fn delete_project(
        &self,
        Parameters(DeleteProjectToolArguments { project_directory }): Parameters<
            DeleteProjectToolArguments,
        >,
    ) -> ToolCallAnswer {
        let project_repositories = Arc::clone(&self.project_repositories_open_in_this_runtime);
        let queued = self
            .a_stream_action_off_the_async_runtime(move |operations_on_the_loaded_streams| {
                let queued = operations_on_the_loaded_streams.delete_project(&project_directory)?;
                project_repositories.close_the_project_repository_at(&queued.project_directory);
                Ok(queued)
            })
            .await?;
        serialized_tool_result(
            "delete_project",
            &DeleteProjectToolResult {
                action: queued.action_id,
                project_directory: queued.project_directory,
            },
        )
    }
}

impl LocalApiMcpServerHandler {
    /// Apply the one settings change `change` asks of the project at `project_directory` by
    /// `author` as one transaction, answering how people see the project after it.
    async fn one_project_settings_change_as_a_transaction(
        &self,
        tool_name: &'static str,
        project_directory: PathBuf,
        author: ProjectTransactionAuthor,
        idempotency_key: Option<String>,
        change: ProjectSettingsChangeAskedOfTheProject,
    ) -> ToolCallAnswer {
        let project_repositories = Arc::clone(&self.project_repositories_open_in_this_runtime);
        let answered = self
            .a_project_repository_call_off_the_async_runtime(
                tool_name,
                project_directory,
                move |project_repository| {
                    let idempotency_key = the_idempotency_key_a_caller_sent(idempotency_key)?;
                    let operation = ProjectOperation::ProjectSettings(change(project_repository));
                    let outcome = project_repository
                        .apply_project_operations(
                            &author,
                            idempotency_key.as_ref(),
                            vec![operation],
                            ProjectTransactionLineage::NewEdit,
                        )
                        .inspect_err(|_| {
                            project_repositories.announce_a_refused_action(
                                project_repository.project_file().project,
                            );
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
            .await?;
        serialized_tool_result(tool_name, &answered)
    }
}
