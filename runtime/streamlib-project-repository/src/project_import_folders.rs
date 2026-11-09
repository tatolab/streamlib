// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::project_asset::{PROJECT_IMPORTS_DIRECTORY, read_every_asset_sidecar};
use crate::project_file::TatolabProjectFile;
use crate::project_repository_refusal::ProjectRepositoryRefusal;

/// The folder holding the import of `project` as the project at `project_directory` sees it:
/// one of its own imports first, then one nested in an import, nearest first.
pub fn the_folder_of_an_import(
    project_directory: &Path,
    project: Uuid,
) -> Result<PathBuf, ProjectRepositoryRefusal> {
    let mut projects_to_read = VecDeque::from([project_directory.to_path_buf()]);
    let mut projects_read = BTreeSet::new();
    while let Some(importing_directory) = projects_to_read.pop_front() {
        let Ok(importing_project) =
            TatolabProjectFile::read_from_project_directory(&importing_directory)
        else {
            continue;
        };
        if !projects_read.insert(importing_project.project) {
            continue;
        }
        for import in &importing_project.dependencies {
            let import_folder = importing_directory
                .join(PROJECT_IMPORTS_DIRECTORY)
                .join(&import.folder);
            if import.project == project {
                return Ok(import_folder);
            }
            projects_to_read.push_back(import_folder);
        }
    }
    Err(ProjectRepositoryRefusal::NoSuchImport(project))
}

/// The file of the asset `asset` of the import `from`, as the project at `project_directory`
/// resolves it: the file at the asset's path inside the import's folder. Refused when no
/// import holds the asset, or when its file is missing, naming the file it needs.
pub fn the_file_of_an_imported_asset(
    project_directory: &Path,
    from: Uuid,
    asset: Uuid,
) -> Result<PathBuf, ProjectRepositoryRefusal> {
    let import_folder = the_folder_of_an_import(project_directory, from)?;
    let held_asset = read_every_asset_sidecar(&import_folder)?
        .remove(&asset)
        .ok_or_else(|| ProjectRepositoryRefusal::ImportHoldsNoAsset {
            project: from,
            asset,
            import_folder: import_folder.clone(),
        })?;
    let file = held_asset.path.on_disk_under(&import_folder);
    if !file.is_file() {
        return Err(ProjectRepositoryRefusal::AssetFileMissing { asset, file });
    }
    Ok(file)
}
