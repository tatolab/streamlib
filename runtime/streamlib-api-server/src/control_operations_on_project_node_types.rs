// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The operation listing the node types a project offers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use streamlib::sdk::runtime::{
    ImportedProjectPythonSources, list_a_projects_node_types_and_graph_generators_from_source,
};
use streamlib_project_repository::{PROJECT_IMPORTS_DIRECTORY, TatolabProjectFile};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ListProjectNodeTypesToolResult, NodeModuleNotReadable, NodeTypeSourceOrigin,
    NodeTypeSourceReadWithoutRunningIt,
};
use uuid::Uuid;

use crate::control_operation_dispatcher::{ControlOperationDispatcher, ControlOperationRefusal};
use crate::project_node_types_followed_for_changes::{
    NodeTypeListingStarted, NodeTypesChangedSinceTheLastListing,
};

const PYTHON_SOURCE_FILE_SUFFIX: &str = ".py";

/// The start of every path inside an import that lies in the import's own `imports/`.
const NESTED_IMPORTS_DIRECTORY_PREFIX: &str = "imports/";

/// What the listing needs from the project's repository, read under its writer.
struct ProjectReadForItsNodeTypeListing {
    project_directory: PathBuf,
    project: Uuid,
    project_name: String,
    import_sources: Vec<ImportedProjectPythonSources>,
    what_the_runtime_read_of_each_import: BTreeMap<String, OneImportReadForItsNodeTypeListing>,
}

/// What the runtime itself reads of one import, beside the sources its interpreter lists.
struct OneImportReadForItsNodeTypeListing {
    /// The import's name as the project file in its folder holds it; `None` when that file
    /// does not read.
    project_name: Option<String>,
    unread_files: Vec<NodeModuleNotReadable>,
}

/// The sources the project's interpreter lists for the project in `project_directory`, read off
/// the async runtime.
async fn the_node_type_sources_the_projects_interpreter_lists(
    project_directory: PathBuf,
    import_sources: Vec<ImportedProjectPythonSources>,
) -> Result<Vec<NodeTypeSourceReadWithoutRunningIt>, String> {
    tokio::task::spawn_blocking(move || {
        list_a_projects_node_types_and_graph_generators_from_source(
            &project_directory,
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
    })
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
        let listing_started = self.project_node_types_last_listed.start_a_listing();
        let project_read = self
            .a_project_repository_call_off_the_async_runtime(
                "list_project_node_types",
                project_directory,
                |project_repository| {
                    let mut import_sources = Vec::new();
                    let mut what_the_runtime_read_of_each_import = BTreeMap::new();
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
                        let import_project = import.project.to_string();
                        what_the_runtime_read_of_each_import.insert(
                            import_project.clone(),
                            OneImportReadForItsNodeTypeListing {
                                project_name: TatolabProjectFile::read_from_project_directory(
                                    &import_folder,
                                )
                                .ok()
                                .map(|import_project_file| import_project_file.name),
                                unread_files,
                            },
                        );
                        import_sources.push(ImportedProjectPythonSources {
                            project: import_project,
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
                        what_the_runtime_read_of_each_import,
                    })
                },
            )
            .await?;
        let mut sources = the_node_type_sources_the_projects_interpreter_lists(
            project_read.project_directory.clone(),
            project_read.import_sources,
        )
        .await
        .inspect_err(|_| {
            self.take_in_the_refusal_of_the_node_type_listing(
                project_read.project,
                listing_started,
                &project_read.project_directory,
            )
        })?;
        let mut what_the_runtime_read_of_each_import =
            project_read.what_the_runtime_read_of_each_import;
        for source in &mut sources {
            if let NodeTypeSourceOrigin::Dependency {
                project,
                project_name,
                ..
            } = &mut source.origin
                && let Some(read_of_the_import) =
                    what_the_runtime_read_of_each_import.remove(project)
            {
                *project_name = read_of_the_import.project_name;
                source
                    .unreadable_modules
                    .extend(read_of_the_import.unread_files);
            }
        }
        let listing = ListProjectNodeTypesToolResult {
            project_directory: project_read.project_directory,
            project: project_read.project.to_string(),
            project_name: project_read.project_name,
            sources,
        };
        self.take_in_the_node_type_listing(project_read.project, listing_started, &listing);
        Ok(listing)
    }

    /// Keep `listing`, started at `listing_started`, as `project`'s last, announce on its feed what
    /// it changed from the one before, and follow the feed for the changes that list it again.
    fn take_in_the_node_type_listing(
        &self,
        project: Uuid,
        listing_started: NodeTypeListingStarted,
        listing: &ListProjectNodeTypesToolResult,
    ) {
        self.project_node_types_last_listed.take_in_the_listing(
            project,
            listing_started,
            listing,
            |NodeTypesChangedSinceTheLastListing {
                 added,
                 removed,
                 updated,
             }| {
                self.project_repositories
                    .announce_the_node_types_changed(project, added, removed, updated)
            },
        );
        self.project_node_types_last_listed
            .follow_the_change_feed_once(self);
    }

    /// Keep that `project`'s listing started at `listing_started` was refused, and follow the
    /// feed and the stream actions for what lists it again.
    fn take_in_the_refusal_of_the_node_type_listing(
        &self,
        project: Uuid,
        listing_started: NodeTypeListingStarted,
        project_directory: &Path,
    ) {
        self.project_node_types_last_listed
            .take_in_the_refusal_of_a_listing(project, listing_started, project_directory);
        self.project_node_types_last_listed
            .follow_the_change_feed_once(self);
    }
}
