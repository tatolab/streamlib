// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The project archive tool: a new project unpacked from a `.zip` of one on this machine.

use std::path::PathBuf;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::schemars::JsonSchema;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};
use serde::Deserialize;
use streamlib_project_repository::ProjectRepository;
use streamlib_runtime_client_contract::local_api_wire_contract::ImportProjectToolResult;

use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer, serialized_tool_result};
use crate::mcp_project_asset_tools::{
    AUTHOR_ARGUMENT_DESCRIPTION, ProjectTransactionAuthorToolArgument, the_author_of_a_change,
};

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ImportProjectToolArguments {
    #[schemars(
        description = "The `.zip` of a project on this machine, by absolute path, such as a GitHub archive downloaded first. It must hold a `tatolab-project.json` with a project UUID, at its root or in its one top folder."
    )]
    source: String,
    #[schemars(
        description = "Where the new project is made, by absolute path: a folder that does not exist yet, its parent made when missing."
    )]
    project_directory: PathBuf,
    #[schemars(description = AUTHOR_ARGUMENT_DESCRIPTION)]
    author: Option<ProjectTransactionAuthorToolArgument>,
}

#[tool_router(router = project_archive_tool_router, vis = "pub(crate)")]
impl LocalApiMcpServerHandler {
    #[tool(
        description = "Make a new project in `project_directory` from the `.zip` of a project at `source`, as a Marketplace entry or a GitHub archive is handed over. Its files are unpacked as `add_dependency` copies a zip — every file its history would record and every asset, its `imports/` included, never a hidden folder such as its `.tatolab/`, a cache, a Python virtual environment or a symbolic link — under a new project UUID of its own, so the same zip imported twice makes two projects, and committed by `author` as the first commit of a history of its own. Nothing reaches the network and nothing runs: follow it with `run_project`. Answers the new `project_directory`, its `project` UUID, its `name` and the `commit`. Refused, nothing left behind, for a relative path, a `source` that is a folder or no zip archive, a zip holding no `tatolab-project.json` or no project UUID in it, naming what is missing, or a `project_directory` that already exists."
    )]
    async fn import_project(
        &self,
        tool_call_request_context: RequestContext<RoleServer>,
        Parameters(ImportProjectToolArguments {
            source,
            project_directory,
            author,
        }): Parameters<ImportProjectToolArguments>,
    ) -> ToolCallAnswer {
        let author = the_author_of_a_change(author, &tool_call_request_context);
        let answered = tokio::task::spawn_blocking(move || {
            let imported = ProjectRepository::import_from_a_project_archive(
                &project_directory,
                &source,
                &author,
            )
            .map_err(|refusal| refusal.to_string())?;
            let commit = imported
                .history()
                .head_commit()
                .map_err(|refusal| refusal.to_string())?
                .ok_or_else(|| "the new project's history holds no first commit".to_owned())?;
            Ok::<_, String>(ImportProjectToolResult {
                project_directory: std::fs::canonicalize(&project_directory)
                    .unwrap_or(project_directory),
                project: imported.project_file().project.to_string(),
                name: imported.project_file().name.clone(),
                commit,
            })
        })
        .await
        .map_err(|join_failure| {
            format!("import_project failed: the call did not finish: {join_failure}")
        })?
        .map_err(|refusal| format!("import_project failed: {refusal}"))?;
        serialized_tool_result("import_project", &answered)
    }
}
