// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The node type listing: what a project, its imports and its installed
//! distributions declare, read from source without running any of it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::schemars::JsonSchema;
use rmcp::{tool, tool_router};
use serde::Deserialize;
use streamlib::sdk::runtime::{
    ImportedProjectPythonSources, list_a_projects_node_types_and_graph_generators_from_source,
};
use streamlib_project_repository::PROJECT_IMPORTS_DIRECTORY;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ListProjectNodeTypesToolResult, NodeTypeSourceReadWithoutRunningIt,
};

use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer, serialized_tool_result};
use crate::mcp_project_asset_tools::ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION;

const PYTHON_SOURCE_FILE_SUFFIX: &str = ".py";

#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ListProjectNodeTypesToolArguments {
    #[schemars(description = ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION)]
    project_directory: PathBuf,
}

/// What the listing needs from the project's repository, read under its writer.
struct ProjectReadForItsNodeTypeListing {
    project_directory: PathBuf,
    project: String,
    project_name: String,
    import_sources: Vec<ImportedProjectPythonSources>,
}

#[tool_router(router = project_node_type_tool_router, vis = "pub(crate)")]
impl LocalApiMcpServerHandler {
    #[tool(
        description = "List the node types and graph generators a project offers, read from source without running any of its code: its own node code outside `imports/`, each import's as its folder holds it now, and each installed distribution that declares `tatolab.nodes` entry points, each under `sources` by `source` (`project`, `dependency` or `installed_distribution`). A node type has its `type` (what `add_node` takes), `kind` (`process`, `function` or `shader`), `description`, `category`, and `inputs` and `outputs` where its declaration states them, each port's `family` null where source alone does not say. A graph generator has its `generator` (what `import_graph_generator` takes), its docstring as `description`, and its `promoted_config`. A module that does not parse is listed under its source's `unreadable_modules` with why and where; the rest still lists."
    )]
    async fn list_project_node_types(
        &self,
        Parameters(ListProjectNodeTypesToolArguments { project_directory }): Parameters<
            ListProjectNodeTypesToolArguments,
        >,
    ) -> ToolCallAnswer {
        let project_read = self
            .a_project_repository_call_off_the_async_runtime(
                "list_project_node_types",
                project_directory,
                |project_repository| {
                    let mut import_sources = Vec::new();
                    for import in &project_repository.project_file().dependencies {
                        let import_folder = project_repository
                            .project_directory()
                            .join(PROJECT_IMPORTS_DIRECTORY)
                            .join(&import.folder);
                        let mut files = BTreeMap::new();
                        for path in project_repository
                            .files_the_import_carries(import)?
                            .into_iter()
                            .filter(|path| path.ends_with(PYTHON_SOURCE_FILE_SUFFIX))
                        {
                            let bytes = std::fs::read(import_folder.join(&path))?;
                            files.insert(path, String::from_utf8_lossy(&bytes).into_owned());
                        }
                        import_sources.push(ImportedProjectPythonSources {
                            project: import.project.to_string(),
                            folder: import.folder.clone(),
                            version: import.version.clone(),
                            files,
                        });
                    }
                    Ok(ProjectReadForItsNodeTypeListing {
                        project_directory: project_repository.project_directory().to_path_buf(),
                        project: project_repository.project_file().project.to_string(),
                        project_name: project_repository.project_file().name.clone(),
                        import_sources,
                    })
                },
            )
            .await?;
        let listing_project_directory = project_read.project_directory.clone();
        let import_sources = project_read.import_sources;
        let sources = tokio::task::spawn_blocking(move || {
            list_a_projects_node_types_and_graph_generators_from_source(
                &listing_project_directory,
                &import_sources,
            )
        })
        .await
        .map_err(|join_failure| {
            format!("list_project_node_types failed: the listing did not finish: {join_failure}")
        })?
        .map_err(|refusal| format!("list_project_node_types failed: {refusal}"))?
        .into_iter()
        .map(serde_json::from_value::<NodeTypeSourceReadWithoutRunningIt>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|not_a_source| {
            format!(
                "list_project_node_types failed: the project's interpreter listed a source this \
                 runtime does not read ({not_a_source}); its `tatolab-stream` may differ from \
                 the runtime's"
            )
        })?;
        serialized_tool_result(
            "list_project_node_types",
            &ListProjectNodeTypesToolResult {
                project_directory: project_read.project_directory,
                project: project_read.project,
                project_name: project_read.project_name,
                sources,
            },
        )
    }
}
