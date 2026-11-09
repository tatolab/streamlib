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
    ListProjectNodeTypesToolResult, NodeModuleNotReadable, NodeTypeSourceOrigin,
    NodeTypeSourceReadWithoutRunningIt,
};

use crate::mcp::{LocalApiMcpServerHandler, ToolCallAnswer, serialized_tool_result};
use crate::mcp_project_asset_tools::ASSET_PROJECT_DIRECTORY_ARGUMENT_DESCRIPTION;

const PYTHON_SOURCE_FILE_SUFFIX: &str = ".py";

/// The start of every path inside an import that lies in the import's own `imports/`.
const NESTED_IMPORTS_DIRECTORY_PREFIX: &str = "imports/";

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
    unread_files_of_each_import: BTreeMap<String, Vec<NodeModuleNotReadable>>,
}

/// The Python import name of the module at `path`, relative to its source's root.
fn the_module_name_of_a_python_file(path: &str) -> String {
    let without_suffix = path.strip_suffix(PYTHON_SOURCE_FILE_SUFFIX).unwrap_or(path);
    let package_or_module = without_suffix
        .strip_suffix("/__init__")
        .unwrap_or(without_suffix);
    package_or_module.replace('/', ".")
}

#[tool_router(router = project_node_type_tool_router, vis = "pub(crate)")]
impl LocalApiMcpServerHandler {
    #[tool(
        description = "List the node types and graph generators a project offers, read from source without running any of its code: its own node code outside `imports/`, each import's as its folder holds it now, and each installed distribution that declares `tatolab.nodes` entry points, each under `sources` by `source` (`project`, `dependency` or `installed_distribution`). A node type has its `type` (what `add_node` takes; an import's is led by its folder, `imports/<folder>/<module>:<class>`, and imported from there), `kind` (`process`, `function` or `shader`), `description`, `category`, and `inputs` and `outputs` where its declaration states them, each port's `family` null where source alone does not say. A graph generator has its `generator` (what `import_graph_generator` takes), its docstring as `description`, and its `promoted_config`. A module that does not parse is listed under its source's `unreadable_modules` with why and where; the rest still lists."
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
                    let mut unread_files_of_each_import = BTreeMap::new();
                    for import in &project_repository.project_file().dependencies {
                        let import_folder = project_repository
                            .project_directory()
                            .join(PROJECT_IMPORTS_DIRECTORY)
                            .join(&import.folder);
                        let mut files = BTreeMap::new();
                        let mut unread_files = Vec::new();
                        for path in project_repository
                            .files_the_import_carries(import)?
                            .into_iter()
                            .filter(|path| {
                                path.ends_with(PYTHON_SOURCE_FILE_SUFFIX)
                                    && !path.starts_with(NESTED_IMPORTS_DIRECTORY_PREFIX)
                            })
                        {
                            match std::fs::read(import_folder.join(&path)) {
                                Ok(bytes) => {
                                    files
                                        .insert(path, String::from_utf8_lossy(&bytes).into_owned());
                                }
                                Err(not_read) => unread_files.push(NodeModuleNotReadable {
                                    module: the_module_name_of_a_python_file(&path),
                                    path,
                                    line: None,
                                    column: None,
                                    why: not_read.to_string(),
                                }),
                            }
                        }
                        unread_files_of_each_import
                            .insert(import.project.to_string(), unread_files);
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
                        unread_files_of_each_import,
                    })
                },
            )
            .await?;
        let listing_project_directory = project_read.project_directory.clone();
        let import_sources = project_read.import_sources;
        let mut unread_files_of_each_import = project_read.unread_files_of_each_import;
        let mut sources = tokio::task::spawn_blocking(move || {
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
        for source in &mut sources {
            if let NodeTypeSourceOrigin::Dependency { project, .. } = &source.origin
                && let Some(unread_files) = unread_files_of_each_import.remove(project)
            {
                source.unreadable_modules.extend(unread_files);
            }
        }
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
