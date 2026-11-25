// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The operation listing the node types a project offers, and the one listing from source it
//! and `list_node_types` both read.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use streamlib::sdk::runtime::{
    ImportedProjectPythonSources, list_a_projects_node_types_and_graph_generators_from_source,
};
use streamlib_project_repository::{PROJECT_IMPORTS_DIRECTORY, TatolabProjectFile};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    NodeModuleNotReadable, NodeTypeSourceOrigin, NodeTypeSourceReadWithoutRunningIt,
    ProjectNodeTypesReadFromSource,
};
use uuid::Uuid;

use crate::control_operation_dispatcher::{ControlOperationDispatcher, ControlOperationRefusal};
use crate::node_type_groups_of_a_project::{NodeTypeGroupsOfAProject, NodeTypeGroupsOfAnImport};
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
    node_type_groups: NodeTypeGroupsOfAProject,
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
    method_name: &'static str,
    project_directory: PathBuf,
    import_sources: Vec<ImportedProjectPythonSources>,
) -> Result<Vec<NodeTypeSourceReadWithoutRunningIt>, ControlOperationRefusal> {
    tokio::task::spawn_blocking(move || {
        list_a_projects_node_types_and_graph_generators_from_source(
            &project_directory,
            &import_sources,
        )
    })
    .await
    .map_err(ControlOperationRefusal::the_task_did_not_finish(format!(
        "{method_name} failed: the listing"
    )))?
    .map_err(|refusal| format!("{method_name} failed: {refusal}"))?
    .into_iter()
    .map(serde_json::from_value::<NodeTypeSourceReadWithoutRunningIt>)
    .collect::<Result<Vec<_>, _>>()
    .map_err(|not_a_source| {
        ControlOperationRefusal::failed(format!(
            "{method_name} failed: the project's interpreter listed a source this runtime does \
             not read ({not_a_source}); its `tatolab-stream` may differ from the runtime's"
        ))
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

impl ControlOperationDispatcher {
    /// The node types and graph generators the project in `project_directory` offers, by
    /// source, read from source without running it for the operation `method_name`, each filed
    /// under the group the project's and its imports' maps or its declaration put it in: kept as
    /// the project's last listing, with what it changed announced on the project's feed and the
    /// feed followed for the changes that list it again.
    pub(crate) async fn the_node_types_the_project_offers_read_from_source(
        &self,
        method_name: &'static str,
        project_directory: PathBuf,
    ) -> Result<ProjectNodeTypesReadFromSource, ControlOperationRefusal> {
        self.the_node_types_the_project_offers_and_its_groups(method_name, project_directory)
            .await
            .map(|(listing, _)| listing)
    }

    /// The listing `the_node_types_the_project_offers_read_from_source` answers, with the node
    /// type to group maps of the project and its imports that filed it, which also file the
    /// node types the runtime brings itself.
    pub(crate) async fn the_node_types_the_project_offers_and_its_groups(
        &self,
        method_name: &'static str,
        project_directory: PathBuf,
    ) -> Result<(ProjectNodeTypesReadFromSource, NodeTypeGroupsOfAProject), ControlOperationRefusal>
    {
        let listing_started = self.project_node_types_last_listed.start_a_listing();
        let project_read = self
            .a_project_repository_call_off_the_async_runtime(
                method_name,
                project_directory,
                |project_repository| {
                    let mut import_sources = Vec::new();
                    let mut what_the_runtime_read_of_each_import = BTreeMap::new();
                    let mut node_type_groups = NodeTypeGroupsOfAProject {
                        own: project_repository.project_file().node_type_groups.clone(),
                        of_each_import: Vec::new(),
                    };
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
                        let (import_project_name, import_node_type_groups) =
                            match TatolabProjectFile::read_from_project_directory(&import_folder) {
                                Ok(TatolabProjectFile {
                                    name,
                                    node_type_groups,
                                    ..
                                }) => (Some(name), node_type_groups),
                                Err(_) => (None, BTreeMap::new()),
                            };
                        if !import_node_type_groups.is_empty() {
                            node_type_groups
                                .of_each_import
                                .push(NodeTypeGroupsOfAnImport {
                                    folder: import.folder.clone(),
                                    groups: import_node_type_groups,
                                });
                        }
                        what_the_runtime_read_of_each_import.insert(
                            import_project.clone(),
                            OneImportReadForItsNodeTypeListing {
                                project_name: import_project_name,
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
                        node_type_groups,
                    })
                },
            )
            .await?;
        let mut sources = the_node_type_sources_the_projects_interpreter_lists(
            method_name,
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
            for read in &mut source.node_types {
                project_read.node_type_groups.file(
                    &read.node_type,
                    &mut read.category,
                    &mut read.category_from,
                );
            }
        }
        let listing = ProjectNodeTypesReadFromSource {
            project_directory: project_read.project_directory,
            project: project_read.project.to_string(),
            project_name: project_read.project_name,
            sources,
        };
        self.take_in_the_node_type_listing(project_read.project, listing_started, &listing);
        Ok((listing, project_read.node_type_groups))
    }

    /// Keep `listing`, started at `listing_started`, as `project`'s last, announce on its feed what
    /// it changed from the one before, and follow the feed for the changes that list it again.
    fn take_in_the_node_type_listing(
        &self,
        project: Uuid,
        listing_started: NodeTypeListingStarted,
        listing: &ProjectNodeTypesReadFromSource,
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
