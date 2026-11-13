// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The Tato Studio views a project and its imports bring: each folder of their `views/`.

use std::path::{Path, PathBuf};

use serde_json::Value;
use uuid::Uuid;

use crate::core_project_import::every_import_a_project_resolves;
use crate::project_asset::PROJECT_IMPORTS_DIRECTORY;
use crate::project_file::TatolabProjectFile;
use crate::project_import_folders::is_one_folder_name;
use crate::project_repository_refusal::ProjectRepositoryRefusal;
use crate::tatolab_view_manifest::{TatolabViewManifest, read_tatolab_view_manifest};

/// The folder of a project whose every folder is one view.
pub const PROJECT_VIEWS_DIRECTORY: &str = "views";

/// The manifest every view's folder holds.
pub const TATOLAB_VIEW_MANIFEST_FILE_NAME: &str = "tatolab_view.json";

/// What separates the project's name from the view's folder in a view type's id.
const VIEW_TYPE_SEPARATOR: char = ':';

/// The project a view comes from: the listed project itself, or one of its imports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectBringingAView {
    /// The project's UUID.
    pub project: Uuid,
    /// The project's name, which leads the view type's id.
    pub project_name: String,
    /// Its folder under `imports/`; `None` for the listed project's own views.
    pub import_folder: Option<String>,
}

/// One folder of a `views/`, read without running anything in it.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectViewFolderRead {
    /// The view type's id, `<project name>:<view folder>`.
    pub view_type: String,
    /// The project whose `views/` holds it.
    pub brought_by: ProjectBringingAView,
    /// The view's folder on disk.
    pub folder: PathBuf,
    /// Its `tatolab_view.json` as written, when it is JSON.
    pub manifest_as_written: Option<Value>,
    /// Its manifest read against the view contract, or why the view does not open.
    pub manifest: Result<TatolabViewManifest, String>,
}

/// The view type id of the view in `view_folder_name` brought by the project named `project_name`.
pub fn the_view_type_of(project_name: &str, view_folder_name: &str) -> String {
    format!("{project_name}{VIEW_TYPE_SEPARATOR}{view_folder_name}")
}

/// Every view the project at `project_directory` brings in its own `views/`, then those of each
/// of its imports in the order its project file records them, each `views/` by folder name.
pub fn every_view_a_project_and_its_imports_bring(
    project_directory: &Path,
) -> Result<Vec<ProjectViewFolderRead>, ProjectRepositoryRefusal> {
    let project_file = TatolabProjectFile::read_from_project_directory(project_directory)?;
    let mut views = every_view_in_the_views_of(
        project_directory,
        &ProjectBringingAView {
            project: project_file.project,
            project_name: project_file.name.clone(),
            import_folder: None,
        },
    )?;
    for import in every_import_a_project_resolves(project_directory, &project_file)
        .iter()
        .filter(|import| is_one_folder_name(&import.folder))
    {
        let import_directory = project_directory
            .join(PROJECT_IMPORTS_DIRECTORY)
            .join(&import.folder);
        let imported_project_name =
            match TatolabProjectFile::read_from_project_directory(&import_directory) {
                Ok(imported_project_file) => imported_project_file.name,
                Err(unreadable) => {
                    tracing::warn!(
                        import_folder = %import_directory.display(),
                        %unreadable,
                        "the import's views are not listed: its project file does not read"
                    );
                    continue;
                }
            };
        views.extend(every_view_in_the_views_of(
            &import_directory,
            &ProjectBringingAView {
                project: import.project,
                project_name: imported_project_name,
                import_folder: Some(import.folder.clone()),
            },
        )?);
    }
    Ok(views)
}

/// Each folder of `views/` in `project_directory`, by name; none when it has no `views/`.
fn every_view_in_the_views_of(
    project_directory: &Path,
    brought_by: &ProjectBringingAView,
) -> Result<Vec<ProjectViewFolderRead>, ProjectRepositoryRefusal> {
    let views_directory = project_directory.join(PROJECT_VIEWS_DIRECTORY);
    let entries = match std::fs::read_dir(&views_directory) {
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
    Ok(view_folder_names
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
        view_type: the_view_type_of(&brought_by.project_name, view_folder_name),
        brought_by: brought_by.clone(),
        folder,
        manifest_as_written,
        manifest,
    }
}

#[cfg(test)]
#[path = "project_views_tests.rs"]
mod project_views_tests;
