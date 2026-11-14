// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::core_project_import::every_import_a_project_resolves;
use crate::project_asset::{PROJECT_IMPORTS_DIRECTORY, read_every_asset_sidecar};
use crate::project_file::TatolabProjectFile;
use crate::project_repository_refusal::ProjectRepositoryRefusal;

/// The folder holding the import of `project` as the project at `project_directory` sees it:
/// one of its own imports first, then one nested in an import, nearest first. An import whose
/// recorded folder is not one folder name is passed over.
pub fn the_folder_of_an_import(
    project_directory: &Path,
    project: Uuid,
) -> Result<PathBuf, ProjectRepositoryRefusal> {
    each_folder_of_an_import_nearest_first(project_directory, project)
        .into_iter()
        .next()
        .ok_or(ProjectRepositoryRefusal::NoSuchImport(project))
}

/// Every folder holding a copy of `project` as the project at `project_directory` sees it,
/// nearest first.
fn each_folder_of_an_import_nearest_first(project_directory: &Path, project: Uuid) -> Vec<PathBuf> {
    let mut folders_of_the_import = Vec::new();
    let mut projects_to_read = VecDeque::from([project_directory.to_path_buf()]);
    let mut directories_read = BTreeSet::new();
    while let Some(importing_directory) = projects_to_read.pop_front() {
        let Ok(directory_on_disk) = importing_directory.canonicalize() else {
            continue;
        };
        if !directories_read.insert(directory_on_disk) {
            continue;
        }
        let Ok(importing_project) =
            TatolabProjectFile::read_from_project_directory(&importing_directory)
        else {
            continue;
        };
        for import in every_import_a_project_resolves(&importing_directory, &importing_project)
            .iter()
            .filter(|import| is_one_folder_name(&import.folder))
        {
            let import_folder = importing_directory
                .join(PROJECT_IMPORTS_DIRECTORY)
                .join(&import.folder);
            if import.project == project {
                folders_of_the_import.push(import_folder.clone());
            }
            projects_to_read.push_back(import_folder);
        }
    }
    folders_of_the_import
}

/// Whether `folder` names one folder inside `imports/`: one normal path component, never an
/// absolute path, `.` or `..`.
pub(crate) fn is_one_folder_name(folder: &str) -> bool {
    let mut components = Path::new(folder).components();
    matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(_)), None)
    )
}

/// The file of the asset `asset` of the import `from`, as the project at `project_directory`
/// resolves it: the file at the asset's path inside the nearest copy of the import holding the
/// asset. Refused when no copy holds the asset, or when its file is missing, naming the file it
/// needs.
pub fn the_file_of_an_imported_asset(
    project_directory: &Path,
    from: Uuid,
    asset: Uuid,
) -> Result<PathBuf, ProjectRepositoryRefusal> {
    let folders_of_the_import = each_folder_of_an_import_nearest_first(project_directory, from);
    let Some(nearest_folder) = folders_of_the_import.first() else {
        return Err(ProjectRepositoryRefusal::NoSuchImport(from));
    };
    for import_folder in &folders_of_the_import {
        let Some(held_asset) = read_every_asset_sidecar(import_folder)?.remove(&asset) else {
            continue;
        };
        let file = held_asset.path.on_disk_under(import_folder);
        if !file.is_file() {
            return Err(ProjectRepositoryRefusal::AssetFileMissing { asset, file });
        }
        return Ok(file);
    }
    Err(ProjectRepositoryRefusal::ImportHoldsNoAsset {
        project: from,
        asset,
        import_folder: nearest_folder.clone(),
    })
}
