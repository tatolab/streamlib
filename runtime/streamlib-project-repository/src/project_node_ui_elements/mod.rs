// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The controls and node bodies a project and its imports register: each one's
//! `node_ui/tatolab_node_ui.json`, read with none of its scripts run.

use std::path::Path;

use serde_json::Value;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    NodeUiManifest, ProjectNodeUiElement, ProjectNodeUiElementListed, ProjectNodeUiManifestListed,
    ProjectNodeUiManifestNotRead, ProjectNodeUiModule,
};

use crate::core_project_import::every_import_a_project_resolves;
use crate::node_ui_manifest::{
    NODE_UI_DIRECTORY, NODE_UI_MANIFEST_FILE_NAME, read_node_ui_manifest,
};
use crate::project_asset::PROJECT_IMPORTS_DIRECTORY;
use crate::project_file::TatolabProjectFile;
use crate::project_import_folders::is_one_folder_name;
use crate::project_repository_refusal::ProjectRepositoryRefusal;

/// One module's `node_ui/tatolab_node_ui.json`, read with none of its scripts run.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectNodeUiManifestRead {
    /// The module whose manifest it is.
    pub module: ProjectNodeUiModule,
    /// Its file, relative to the listed project.
    pub manifest_path: String,
    /// The manifest read against the view contract, or why it does not read.
    pub manifest: Result<NodeUiManifest, String>,
}

impl ProjectNodeUiManifestRead {
    /// The path, relative to the listed project, of `script` as the manifest names it, inside
    /// its module's node UI folder.
    pub fn the_path_of_a_script(&self, script: &str) -> String {
        let node_ui_folder = self
            .manifest_path
            .strip_suffix(NODE_UI_MANIFEST_FILE_NAME)
            .unwrap_or(&self.manifest_path);
        format!("{node_ui_folder}{script}")
    }
}

/// What a project and its imports register, as `list_project_node_ui_elements` answers it.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectNodeUiElementsListed {
    /// Each element, the project's own first, then each import's.
    pub elements: Vec<ProjectNodeUiElementListed>,
    /// Each manifest that reads, as written.
    pub manifests: Vec<ProjectNodeUiManifestListed>,
    /// Each manifest that does not read, with its file and why.
    pub unreadable_manifests: Vec<ProjectNodeUiManifestNotRead>,
}

/// Every element the project at `project_directory` registers in its own
/// `node_ui/tatolab_node_ui.json`, then those each of its imports registers, in the order its
/// project file records them. A module with no manifest registers none.
pub fn every_node_ui_element_a_project_and_its_imports_bring(
    project_directory: &Path,
) -> Result<ProjectNodeUiElementsListed, ProjectRepositoryRefusal> {
    let mut listed = ProjectNodeUiElementsListed {
        elements: Vec::new(),
        manifests: Vec::new(),
        unreadable_manifests: Vec::new(),
    };
    for read in every_node_ui_manifest_a_project_and_its_imports_bring(project_directory)? {
        match read.manifest {
            Ok(ref manifest) => {
                listed.elements.extend(
                    manifest
                        .controls
                        .iter()
                        .map(|control| ProjectNodeUiElementListed {
                            module: read.module.clone(),
                            script_path: read.the_path_of_a_script(&control.script),
                            element: ProjectNodeUiElement::Control {
                                pin_type: control.pin_type,
                                control: control.control.clone(),
                            },
                        })
                        .chain(manifest.node_bodies.iter().map(|body| {
                            ProjectNodeUiElementListed {
                                module: read.module.clone(),
                                script_path: read.the_path_of_a_script(&body.script),
                                element: ProjectNodeUiElement::NodeBody {
                                    node_type: body.node_type.clone(),
                                },
                            }
                        })),
                );
                listed.manifests.push(ProjectNodeUiManifestListed {
                    module: read.module,
                    manifest_path: read.manifest_path,
                    manifest: manifest.clone(),
                });
            }
            Err(why) => listed
                .unreadable_manifests
                .push(ProjectNodeUiManifestNotRead {
                    module: read.module,
                    manifest_path: read.manifest_path,
                    why,
                }),
        }
    }
    Ok(listed)
}

/// The `node_ui/tatolab_node_ui.json` of the project at `project_directory`, then that of each
/// of its imports in the order its project file records them; a module with none is left out.
fn every_node_ui_manifest_a_project_and_its_imports_bring(
    project_directory: &Path,
) -> Result<Vec<ProjectNodeUiManifestRead>, ProjectRepositoryRefusal> {
    let project_file = TatolabProjectFile::read_from_project_directory(project_directory)?;
    let mut manifests: Vec<ProjectNodeUiManifestRead> = the_node_ui_manifest_in(
        project_directory,
        "",
        ProjectNodeUiModule {
            project: project_file.project.to_string(),
            project_name: project_file.name.clone(),
            import_folder: None,
        },
    )
    .into_iter()
    .collect();
    for import in every_import_a_project_resolves(project_directory, &project_file)
        .iter()
        .filter(|import| is_one_folder_name(&import.folder))
    {
        let import_directory = project_directory
            .join(PROJECT_IMPORTS_DIRECTORY)
            .join(&import.folder);
        let imported_project_name = match TatolabProjectFile::read_from_project_directory(
            &import_directory,
        ) {
            Ok(imported_project_file) => imported_project_file.name,
            Err(unreadable) => {
                tracing::warn!(
                    import_folder = %import_directory.display(),
                    %unreadable,
                    "the import's node UI elements are not listed: its project file does not read"
                );
                continue;
            }
        };
        manifests.extend(the_node_ui_manifest_in(
            &import_directory,
            &format!("{PROJECT_IMPORTS_DIRECTORY}/{}/", import.folder),
            ProjectNodeUiModule {
                project: import.project.to_string(),
                project_name: imported_project_name,
                import_folder: Some(import.folder.clone()),
            },
        ));
    }
    Ok(manifests)
}

/// The manifest in `module_directory`'s node UI folder, its path led by `path_prefix`, the
/// module's folder relative to the listed project; `None` when there is no such file.
fn the_node_ui_manifest_in(
    module_directory: &Path,
    path_prefix: &str,
    module: ProjectNodeUiModule,
) -> Option<ProjectNodeUiManifestRead> {
    let NodeUiManifestFileRead { manifest, .. } = the_node_ui_manifest_file_read(
        &module_directory
            .join(NODE_UI_DIRECTORY)
            .join(NODE_UI_MANIFEST_FILE_NAME),
    )?;
    Some(ProjectNodeUiManifestRead {
        module,
        manifest_path: format!("{path_prefix}{NODE_UI_DIRECTORY}/{NODE_UI_MANIFEST_FILE_NAME}"),
        manifest,
    })
}

/// A `tatolab_node_ui.json` as read: its bytes when they were read, and the manifest or why it
/// does not read.
pub(crate) struct NodeUiManifestFileRead {
    pub(crate) bytes: Option<Vec<u8>>,
    pub(crate) manifest: Result<NodeUiManifest, String>,
}

/// `manifest_file` read against the view contract; `None` when there is no such file.
pub(crate) fn the_node_ui_manifest_file_read(
    manifest_file: &Path,
) -> Option<NodeUiManifestFileRead> {
    match std::fs::read(manifest_file) {
        Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => None,
        Err(unreadable) => Some(NodeUiManifestFileRead {
            bytes: None,
            manifest: Err(format!(
                "`{NODE_UI_MANIFEST_FILE_NAME}` cannot be read: {unreadable}"
            )),
        }),
        Ok(bytes) => {
            let manifest = serde_json::from_slice::<Value>(&bytes)
                .map_err(|not_json| {
                    format!("`{NODE_UI_MANIFEST_FILE_NAME}` is not JSON: {not_json}")
                })
                .and_then(|manifest_json| read_node_ui_manifest(&manifest_json));
            Some(NodeUiManifestFileRead {
                bytes: Some(bytes),
                manifest,
            })
        }
    }
}

#[cfg(test)]
mod tests;
