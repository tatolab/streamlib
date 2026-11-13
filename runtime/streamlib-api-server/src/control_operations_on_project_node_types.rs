// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The operation listing the node types a project offers.

use std::collections::BTreeMap;
use std::path::PathBuf;

use streamlib::sdk::runtime::{
    ImportedProjectPythonSources, list_a_projects_node_types_and_graph_generators_from_source,
};
use streamlib_project_repository::PROJECT_IMPORTS_DIRECTORY;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ListProjectNodeTypesToolResult, NodeModuleNotReadable, NodeTypeSourceOrigin,
    NodeTypeSourceReadWithoutRunningIt,
};
use uuid::Uuid;

use crate::control_operation_dispatcher::{ControlOperationDispatcher, ControlOperationRefusal};
use crate::project_node_types_followed_for_changes::NodeTypesChangedSinceTheLastListing;

const PYTHON_SOURCE_FILE_SUFFIX: &str = ".py";

/// The start of every path inside an import that lies in the import's own `imports/`.
const NESTED_IMPORTS_DIRECTORY_PREFIX: &str = "imports/";

/// What the listing needs from the project's repository, read under its writer.
struct ProjectReadForItsNodeTypeListing {
    project_directory: PathBuf,
    project: Uuid,
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

use streamlib_control_contract::ListProjectNodeTypesRequest;
impl ControlOperationDispatcher {
    pub(crate) async fn list_project_node_types(
        &self,
        ListProjectNodeTypesRequest { project_directory }: ListProjectNodeTypesRequest,
    ) -> Result<ListProjectNodeTypesToolResult, ControlOperationRefusal> {
        let project_read = self
            .a_project_repository_call_off_the_async_runtime(
                "list_project_node_types",
                project_directory,
                |project_repository| {
                    let mut import_sources = Vec::new();
                    let mut unread_files_of_each_import = BTreeMap::new();
                    for import in &project_repository.every_import_the_project_resolves() {
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
                        project: project_repository.project_file().project,
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
        let listing = ListProjectNodeTypesToolResult {
            project_directory: project_read.project_directory,
            project: project_read.project.to_string(),
            project_name: project_read.project_name,
            sources,
        };
        self.take_in_the_node_type_listing(project_read.project, &listing);
        Ok(listing)
    }

    /// Keep `listing` as `project`'s last, announce on its feed what it changed from the one
    /// before, and follow the feed for the changes that list it again.
    fn take_in_the_node_type_listing(
        &self,
        project: Uuid,
        listing: &ListProjectNodeTypesToolResult,
    ) {
        let NodeTypesChangedSinceTheLastListing {
            added,
            removed,
            updated,
        } = self
            .project_node_types_last_listed
            .take_in_the_listing(project, listing);
        self.project_repositories
            .announce_the_node_types_changed(project, added, removed, updated);
        self.project_node_types_last_listed
            .follow_the_change_feed_once(self);
    }
}
