// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The Tato Studio views a project and its imports bring: each folder of their `views/`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::Value;
use uuid::Uuid;

use crate::core_project_import::every_import_a_project_resolves;
use crate::project_asset::PROJECT_IMPORTS_DIRECTORY;
use crate::project_file::TatolabProjectFile;
use crate::project_import_folders::is_one_folder_name;
use crate::project_repository_refusal::ProjectRepositoryRefusal;
use crate::project_stream_document::project_stream_documents;
use crate::stream_document_upgrade::the_node_type_renamed_by;
use crate::stream_graph_document::StreamGraphDocument;
use crate::tatolab_view_manifest::{TatolabViewManifest, read_tatolab_view_manifest};

/// The folder of a project whose every folder is one view.
pub const PROJECT_VIEWS_DIRECTORY: &str = "views";

/// The manifest every view's folder holds.
pub const TATOLAB_VIEW_MANIFEST_FILE_NAME: &str = "tatolab_view.json";

/// What separates the project's UUID from the view's folder in a view type's id.
const VIEW_TYPE_SEPARATOR: char = ':';

/// The project a view comes from: the listed project itself, or one of its imports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectBringingAView {
    /// The project's UUID.
    pub project: Uuid,
    /// The project's name, which only shows.
    pub project_name: String,
    /// Its folder under `imports/`; `None` for the listed project's own views.
    pub import_folder: Option<String>,
}

/// One folder of a `views/`, read without running anything in it.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectViewFolderRead {
    /// The view type's id, `<project uuid>:<view folder>`.
    pub view_type: String,
    /// Its folder's name in the `views/` of the project bringing it.
    pub view_folder_name: String,
    /// The project whose `views/` holds it.
    pub brought_by: ProjectBringingAView,
    /// The view's folder on disk.
    pub folder: PathBuf,
    /// Its `tatolab_view.json` as written, when it is JSON.
    pub manifest_as_written: Option<Value>,
    /// Its manifest read against the view contract, or why the view does not open.
    pub manifest: Result<TatolabViewManifest, String>,
}

/// The view type id of the view in `view_folder_name` brought by the project `project`.
pub fn the_view_type_of(project: Uuid, view_folder_name: &str) -> String {
    format!("{project}{VIEW_TYPE_SEPARATOR}{view_folder_name}")
}

/// The type a node placing the view in `view_folder_name` of the project named `project_name`
/// takes when it is written `<project name>:<view folder>`.
fn the_view_type_named_by_its_projects_name(project_name: &str, view_folder_name: &str) -> String {
    format!("{project_name}{VIEW_TYPE_SEPARATOR}{view_folder_name}")
}

/// Each view of the project at `project_directory` and its imports whose project's name is one
/// of `project_names_placed`, by `<project name>:<view folder>`, mapped to its type; the
/// project's own views first, then each import's in the order its project file records them,
/// the first view a name and folder name wins.
pub(crate) fn the_type_of_each_view_placed_by_its_projects_name(
    project_directory: &Path,
    project_names_placed: &HashSet<String>,
) -> Result<HashMap<String, String>, ProjectRepositoryRefusal> {
    let mut type_of_each_view_placed_by_its_projects_name = HashMap::new();
    for (brought_by, its_directory) in every_project_bringing_views(project_directory)? {
        if !project_names_placed.contains(&brought_by.project_name) {
            continue;
        }
        for view_folder_name in the_view_folder_names_in(&its_directory)? {
            type_of_each_view_placed_by_its_projects_name
                .entry(the_view_type_named_by_its_projects_name(
                    &brought_by.project_name,
                    &view_folder_name,
                ))
                .or_insert_with(|| the_view_type_of(brought_by.project, &view_folder_name));
        }
    }
    Ok(type_of_each_view_placed_by_its_projects_name)
}

/// The type each of the project at `project_directory`'s own views had in the project it was
/// copied from, `project_before` named `name_before`, by that project's UUID and by its name,
/// mapped to the type it has under the UUID its project file gives it now.
fn the_type_each_own_view_had_in_the_project_copied(
    project_directory: &Path,
    project_before: Uuid,
    name_before: &str,
) -> Result<HashMap<String, String>, ProjectRepositoryRefusal> {
    let project = TatolabProjectFile::read_from_project_directory(project_directory)?.project;
    let mut type_now_of_each_type_before = HashMap::new();
    for view_folder_name in the_view_folder_names_in(project_directory)? {
        let type_now = the_view_type_of(project, &view_folder_name);
        for type_before in [
            the_view_type_of(project_before, &view_folder_name),
            the_view_type_named_by_its_projects_name(name_before, &view_folder_name),
        ] {
            if type_before != type_now {
                type_now_of_each_type_before.insert(type_before, type_now.clone());
            }
        }
    }
    Ok(type_now_of_each_type_before)
}

/// Retype each node of the graph documents of the project at `project_directory`, a copy of the
/// project `project_before` named `name_before`, that places one of the copy's own views by
/// that project's UUID or name, to the view's type under the UUID the copy's project file gives
/// it, so the copy's streams place its own views. Only a graph document at a project-relative
/// path `the_copy_wrote` holds is retyped; one that does not read is left as it was copied.
pub(crate) fn place_each_own_view_of_a_copy_by_its_type_in_the_copy(
    project_directory: &Path,
    project_before: Uuid,
    name_before: &str,
    the_copy_wrote: impl Fn(&str) -> bool,
) -> Result<(), ProjectRepositoryRefusal> {
    let retyped = the_type_each_own_view_had_in_the_project_copied(
        project_directory,
        project_before,
        name_before,
    )?;
    if retyped.is_empty() {
        return Ok(());
    }
    for stream_document in project_stream_documents(project_directory)?
        .into_iter()
        .filter(|stream_document| the_copy_wrote(stream_document.path.as_str()))
    {
        let Some(mut graph_document) = stream_document
            .bytes
            .as_ref()
            .ok()
            .and_then(|bytes| StreamGraphDocument::from_bytes(bytes).ok())
        else {
            tracing::debug!(
                stream_document = %stream_document.path,
                "the copy's graph document does not read; it is left as it was copied"
            );
            continue;
        };
        if retype_each_node_at_every_level(graph_document.node_values_mut(), &retyped) {
            std::fs::write(
                stream_document.path.on_disk_under(project_directory),
                graph_document.canonical_bytes()?,
            )?;
        }
    }
    Ok(())
}

/// Give each node of `node_values`, and of each fragment one of them places, whose type is a key
/// of `retyped` the type it maps to; whether any changed.
pub(crate) fn retype_each_node_at_every_level(
    node_values: &mut [Value],
    retyped: &HashMap<String, String>,
) -> bool {
    let mut any_retyped = false;
    for node_object in node_values.iter_mut().filter_map(Value::as_object_mut) {
        any_retyped |= the_node_type_renamed_by(node_object, |node_type| {
            retyped.get(node_type).map(String::as_str)
        });
        if let Some(fragment_nodes) = node_object
            .get_mut("fragment")
            .and_then(|fragment| fragment.get_mut("nodes"))
            .and_then(Value::as_array_mut)
        {
            any_retyped |= retype_each_node_at_every_level(fragment_nodes, retyped);
        }
    }
    any_retyped
}

/// Every view the project at `project_directory` brings in its own `views/`, then those of each
/// of its imports in the order its project file records them, each `views/` by folder name.
pub fn every_view_a_project_and_its_imports_bring(
    project_directory: &Path,
) -> Result<Vec<ProjectViewFolderRead>, ProjectRepositoryRefusal> {
    let mut views = Vec::new();
    for (brought_by, its_directory) in every_project_bringing_views(project_directory)? {
        views.extend(every_view_in_the_views_of(&its_directory, &brought_by)?);
    }
    Ok(views)
}

/// The project at `project_directory`, then each of its imports whose project file reads, in
/// the order its project file records them, each with the directory it is in.
fn every_project_bringing_views(
    project_directory: &Path,
) -> Result<Vec<(ProjectBringingAView, PathBuf)>, ProjectRepositoryRefusal> {
    let project_file = TatolabProjectFile::read_from_project_directory(project_directory)?;
    let mut projects = vec![(
        ProjectBringingAView {
            project: project_file.project,
            project_name: project_file.name.clone(),
            import_folder: None,
        },
        project_directory.to_path_buf(),
    )];
    for import in every_import_a_project_resolves(project_directory, &project_file)
        .iter()
        .filter(|import| is_one_folder_name(&import.folder))
    {
        let import_directory = project_directory
            .join(PROJECT_IMPORTS_DIRECTORY)
            .join(&import.folder);
        match TatolabProjectFile::read_from_project_directory(&import_directory) {
            Ok(imported_project_file) => projects.push((
                ProjectBringingAView {
                    project: import.project,
                    project_name: imported_project_file.name,
                    import_folder: Some(import.folder.clone()),
                },
                import_directory,
            )),
            Err(unreadable) => tracing::warn!(
                import_folder = %import_directory.display(),
                %unreadable,
                "the import's views are not listed: its project file does not read"
            ),
        }
    }
    Ok(projects)
}

/// Every view the project at `project_directory` brings in its own `views/`, by folder name,
/// reading none of its imports.
#[cfg(feature = "history")]
pub(crate) fn every_view_a_project_brings_itself(
    project_directory: &Path,
) -> Result<Vec<ProjectViewFolderRead>, ProjectRepositoryRefusal> {
    let project_file = TatolabProjectFile::read_from_project_directory(project_directory)?;
    every_view_in_the_views_of(
        project_directory,
        &ProjectBringingAView {
            project: project_file.project,
            project_name: project_file.name,
            import_folder: None,
        },
    )
}

/// Each folder of `views/` in `project_directory`, by name; none when it has no `views/`.
fn every_view_in_the_views_of(
    project_directory: &Path,
    brought_by: &ProjectBringingAView,
) -> Result<Vec<ProjectViewFolderRead>, ProjectRepositoryRefusal> {
    let views_directory = project_directory.join(PROJECT_VIEWS_DIRECTORY);
    Ok(the_view_folder_names_in(project_directory)?
        .into_iter()
        .map(|view_folder_name| {
            the_view_folder_read(
                views_directory.join(&view_folder_name),
                &view_folder_name,
                brought_by,
            )
        })
        .collect())
}

/// The name of each folder of `views/` in `project_directory`, sorted, reading none of them;
/// none when it has no `views/`.
fn the_view_folder_names_in(
    project_directory: &Path,
) -> Result<Vec<String>, ProjectRepositoryRefusal> {
    let entries = match std::fs::read_dir(project_directory.join(PROJECT_VIEWS_DIRECTORY)) {
        Ok(entries) => entries,
        Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(unreadable) => return Err(unreadable.into()),
    };
    let mut view_folder_names = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(view_folder_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if view_folder_name.starts_with('.') {
            continue;
        }
        view_folder_names.push(view_folder_name);
    }
    view_folder_names.sort();
    Ok(view_folder_names)
}

fn the_view_folder_read(
    folder: PathBuf,
    view_folder_name: &str,
    brought_by: &ProjectBringingAView,
) -> ProjectViewFolderRead {
    let (manifest_as_written, manifest) =
        match std::fs::read(folder.join(TATOLAB_VIEW_MANIFEST_FILE_NAME)) {
            Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => (
                None,
                Err(format!(
                    "the view's folder has no `{TATOLAB_VIEW_MANIFEST_FILE_NAME}`"
                )),
            ),
            Err(unreadable) => (
                None,
                Err(format!(
                    "`{TATOLAB_VIEW_MANIFEST_FILE_NAME}` could not be read: {unreadable}"
                )),
            ),
            Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Err(not_json) => (
                    None,
                    Err(format!(
                        "`{TATOLAB_VIEW_MANIFEST_FILE_NAME}` is not JSON: {not_json}"
                    )),
                ),
                Ok(manifest_json) => {
                    let manifest = read_tatolab_view_manifest(&manifest_json);
                    (Some(manifest_json), manifest)
                }
            },
        };
    ProjectViewFolderRead {
        view_type: the_view_type_of(brought_by.project, view_folder_name),
        view_folder_name: view_folder_name.to_owned(),
        brought_by: brought_by.clone(),
        folder,
        manifest_as_written,
        manifest,
    }
}

#[cfg(test)]
#[path = "project_views_tests.rs"]
mod project_views_tests;
