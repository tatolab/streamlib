// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::canonical_project_json::canonical_project_json_bytes;
use crate::project_asset::{DIRECTORIES_A_PROJECT_SCAN_SKIPS, PROJECT_IMPORTS_DIRECTORY};
use crate::project_file::PROJECT_FILE_NAME;
use crate::project_repository_refusal::ProjectRepositoryRefusal;

/// Where a project stages each import's files before a transaction moves them into place, and
/// keeps an import's folder a transaction takes out until it commits, relative to the project.
pub(crate) const PROJECT_IMPORT_STAGING_DIRECTORY: &str = ".tatolab/staged-imports";

/// The file that makes a folder a Python virtual environment.
const PYVENV_CONFIG_FILE_NAME: &str = "pyvenv.cfg";

/// Finder writes this file into any folder a person opens on macOS.
const FINDER_FOLDER_METADATA_FILE_NAME: &str = ".DS_Store";

/// A project copied, whole, into the importing project's staging folder.
#[derive(Clone, Debug)]
pub(crate) struct StagedProjectImportCopy {
    /// Where the copy sits, relative to the importing project.
    pub(crate) staged_at: String,
    /// The copied project's UUID.
    pub(crate) project: Uuid,
    /// The copied project's name.
    pub(crate) name: String,
    /// The SHA-256 of each file the copy carries, by its path inside the copy.
    pub(crate) copied_files: BTreeMap<String, String>,
}

impl StagedProjectImportCopy {
    /// The SHA-256 naming the version the copy carried: of its files' paths and hashes.
    pub(crate) fn version(&self) -> Result<String, ProjectRepositoryRefusal> {
        version_of_the_copied_files(&self.copied_files)
    }
}

/// The SHA-256 of `copied_files`' canonical JSON, which names the version of an import.
pub(crate) fn version_of_the_copied_files(
    copied_files: &BTreeMap<String, String>,
) -> Result<String, ProjectRepositoryRefusal> {
    Ok(lowercase_hex_sha256(&canonical_project_json_bytes(
        copied_files,
    )?))
}

/// Copy the project `source` holds into a fresh staging folder of the project at
/// `project_directory`: `source` is a project folder or a `.zip` of one, a GitHub archive's
/// one top folder included, by absolute path. What is copied is every file its history would
/// record and every asset file it holds, its own `imports/` included, never a hidden folder such
/// as its `.tatolab/`, a cache, a Python virtual environment or a symbolic link. Refused, nothing
/// left behind, when the source holds no `tatolab-project.json` naming a project UUID.
pub(crate) fn stage_a_copy_of_the_project_source(
    project_directory: &Path,
    source: &str,
) -> Result<StagedProjectImportCopy, ProjectRepositoryRefusal> {
    let source_path = Path::new(source);
    if !source_path.is_absolute() {
        return Err(ProjectRepositoryRefusal::ImportSourceNotAnAbsolutePath {
            import_source: source.to_owned(),
        });
    }
    let staged_at = format!("{PROJECT_IMPORT_STAGING_DIRECTORY}/{}", Uuid::new_v4());
    let staging_directory = project_directory.join(&staged_at);
    let staged = if source_path.is_dir() {
        copy_the_project_folder(source_path, &staging_directory)
    } else if source_path.is_file() {
        extract_the_project_archive(source_path, &staging_directory)
    } else {
        Err(ProjectRepositoryRefusal::ImportSourceUnreadable {
            path: source_path.to_path_buf(),
            why: "no folder or file is there".to_owned(),
        })
    }
    .and_then(|()| the_project_a_staged_copy_holds(&staging_directory, source))
    .and_then(|(project, name)| {
        Ok(StagedProjectImportCopy {
            copied_files: sha256_of_each_file_under(&staging_directory)?,
            staged_at: staged_at.clone(),
            project,
            name,
        })
    });
    if staged.is_err() {
        let _ = std::fs::remove_dir_all(&staging_directory);
    }
    staged
}

/// The SHA-256 of each file under `directory` an import carries, by its path inside it.
pub(crate) fn sha256_of_each_file_under(
    directory: &Path,
) -> Result<BTreeMap<String, String>, ProjectRepositoryRefusal> {
    let mut hashes = BTreeMap::new();
    for relative in files_an_import_carries(directory)? {
        hashes.insert(
            relative.clone(),
            lowercase_hex_sha256(&std::fs::read(directory.join(&relative))?),
        );
    }
    Ok(hashes)
}

/// Every file under `directory` an import carries, relative and sorted: each regular file
/// outside a hidden folder, a folder a project scan skips and a Python virtual environment.
pub(crate) fn files_an_import_carries(
    directory: &Path,
) -> Result<Vec<String>, ProjectRepositoryRefusal> {
    let mut found = Vec::new();
    let mut directories_to_read = vec![(directory.to_path_buf(), String::new())];
    while let Some((reading, relative_directory)) = directories_to_read.pop() {
        for entry in std::fs::read_dir(&reading)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if relative_directory.is_empty() {
                name.clone()
            } else {
                format!("{relative_directory}/{name}")
            };
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                if is_a_folder_an_import_carries(&name)
                    && !entry.path().join(PYVENV_CONFIG_FILE_NAME).exists()
                {
                    directories_to_read.push((entry.path(), relative));
                }
            } else if file_type.is_file() && is_a_file_an_import_carries(&relative) {
                found.push(relative);
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Each file `edited` holds that differs from `copied_files`: changed, added or taken out.
pub(crate) fn files_edited_since_the_copy(
    copied_files: &BTreeMap<String, String>,
    edited: &BTreeMap<String, String>,
) -> Vec<String> {
    let every_path: BTreeSet<&String> = copied_files.keys().chain(edited.keys()).collect();
    every_path
        .into_iter()
        .filter(|path| is_a_file_an_import_carries(path))
        .filter(|path| copied_files.get(*path) != edited.get(*path))
        .cloned()
        .collect()
}

/// The folder name a copy of the project named `name` takes under `imports/`: its name in
/// lowercase, each run of characters other than letters, digits, `-` and `_` made one `-`.
pub(crate) fn the_import_folder_named_after(name: &str) -> String {
    let mut folder = String::new();
    for character in name.trim().chars() {
        if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
            folder.push(character.to_ascii_lowercase());
        } else if !folder.ends_with('-') {
            folder.push('-');
        }
    }
    let folder = folder.trim_matches('-').to_owned();
    if folder.is_empty() {
        "module".to_owned()
    } else {
        folder
    }
}

/// Refuse `folder` unless it names one folder directly under `imports/`.
pub(crate) fn refuse_an_import_folder_name(folder: &str) -> Result<(), ProjectRepositoryRefusal> {
    let refuse = |why: &'static str| ProjectRepositoryRefusal::ImportFolderRefused {
        folder: folder.to_owned(),
        why,
    };
    if folder.is_empty() {
        return Err(refuse("it is empty"));
    }
    if folder.contains('/') || folder.contains('\\') {
        return Err(refuse("it names one folder directly under `imports/`"));
    }
    if folder.starts_with('.') {
        return Err(refuse("it starts with `.`"));
    }
    if DIRECTORIES_A_PROJECT_SCAN_SKIPS.contains(&folder) {
        return Err(refuse("a project's scan never enters it"));
    }
    Ok(())
}

/// Where the import in `folder` sits in the project at `project_directory`.
pub(crate) fn the_import_folder_in(project_directory: &Path, folder: &str) -> PathBuf {
    project_directory
        .join(PROJECT_IMPORTS_DIRECTORY)
        .join(folder)
}

fn is_a_file_an_import_carries(relative_path: &str) -> bool {
    relative_path.rsplit('/').next() != Some(FINDER_FOLDER_METADATA_FILE_NAME)
}

fn is_a_folder_an_import_carries(name: &str) -> bool {
    !name.starts_with('.') && !DIRECTORIES_A_PROJECT_SCAN_SKIPS.contains(&name)
}

fn copy_the_project_folder(
    source: &Path,
    staging_directory: &Path,
) -> Result<(), ProjectRepositoryRefusal> {
    let unreadable = |why: std::io::Error| ProjectRepositoryRefusal::ImportSourceUnreadable {
        path: source.to_path_buf(),
        why: why.to_string(),
    };
    std::fs::create_dir_all(staging_directory)?;
    for relative in files_an_import_carries(source)? {
        let destination = staging_directory.join(&relative);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(source.join(&relative), destination).map_err(unreadable)?;
    }
    Ok(())
}

fn extract_the_project_archive(
    archive_path: &Path,
    staging_directory: &Path,
) -> Result<(), ProjectRepositoryRefusal> {
    let unreadable = |why: String| ProjectRepositoryRefusal::ImportSourceUnreadable {
        path: archive_path.to_path_buf(),
        why,
    };
    let archive_file =
        std::fs::File::open(archive_path).map_err(|why| unreadable(why.to_string()))?;
    let mut archive = zip::ZipArchive::new(archive_file)
        .map_err(|why| unreadable(format!("it is no folder and no zip archive: {why}")))?;
    let entry_paths: Vec<Option<PathBuf>> = (0..archive.len())
        .map(|index| {
            archive
                .by_index_raw(index)
                .ok()
                .and_then(|entry| entry.enclosed_name())
        })
        .collect();
    let root = the_project_root_inside_the_archive(&entry_paths);
    let virtual_environments: BTreeSet<PathBuf> = entry_paths
        .iter()
        .flatten()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name == PYVENV_CONFIG_FILE_NAME)
        })
        .filter_map(|path| path.parent().map(Path::to_path_buf))
        .filter(|environment| *environment != root)
        .collect();
    std::fs::create_dir_all(staging_directory)?;
    for (index, entry_path) in entry_paths.iter().enumerate() {
        let Some(entry_path) = entry_path else {
            continue;
        };
        let Ok(inside_the_project) = entry_path.strip_prefix(&root) else {
            continue;
        };
        let mut entry = archive
            .by_index(index)
            .map_err(|why| unreadable(why.to_string()))?;
        if !entry.is_file() || entry.is_symlink() {
            continue;
        }
        let in_a_folder_no_import_carries = inside_the_project
            .parent()
            .into_iter()
            .flat_map(Path::components)
            .any(|component| {
                !is_a_folder_an_import_carries(&component.as_os_str().to_string_lossy())
            });
        let in_a_virtual_environment = virtual_environments
            .iter()
            .any(|environment| entry_path.starts_with(environment));
        let is_finder_metadata =
            !is_a_file_an_import_carries(&inside_the_project.to_string_lossy());
        if in_a_folder_no_import_carries || in_a_virtual_environment || is_finder_metadata {
            continue;
        }
        let destination = staging_directory.join(inside_the_project);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|why| unreadable(why.to_string()))?;
        std::fs::write(destination, bytes)?;
    }
    Ok(())
}

/// The folder of the archive the project sits in: its root when it holds `tatolab-project.json`
/// there, else its one top folder, as a GitHub archive wraps a repository.
fn the_project_root_inside_the_archive(entry_paths: &[Option<PathBuf>]) -> PathBuf {
    let holds_the_project_file_at = |root: &Path| {
        entry_paths
            .iter()
            .flatten()
            .any(|path| path == &root.join(PROJECT_FILE_NAME))
    };
    if holds_the_project_file_at(Path::new("")) {
        return PathBuf::new();
    }
    let top_folders: BTreeSet<PathBuf> = entry_paths
        .iter()
        .flatten()
        .filter_map(|path| path.components().next())
        .map(|component| PathBuf::from(component.as_os_str()))
        .collect();
    match top_folders.into_iter().collect::<Vec<_>>().as_slice() {
        [only_top_folder] if holds_the_project_file_at(only_top_folder) => only_top_folder.clone(),
        _ => PathBuf::new(),
    }
}

fn the_project_a_staged_copy_holds(
    staging_directory: &Path,
    source: &str,
) -> Result<(Uuid, String), ProjectRepositoryRefusal> {
    let holds_no_project =
        |what_is_missing: String| ProjectRepositoryRefusal::ImportSourceHoldsNoProject {
            import_source: source.to_owned(),
            what_is_missing,
        };
    let bytes = match std::fs::read(staging_directory.join(PROJECT_FILE_NAME)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(holds_no_project(format!(
                "it has no `{PROJECT_FILE_NAME}` at its root"
            )));
        }
        Err(error) => return Err(error.into()),
    };
    let project_file: Value = serde_json::from_slice(&bytes).map_err(|why| {
        holds_no_project(format!("its `{PROJECT_FILE_NAME}` does not parse: {why}"))
    })?;
    let project = project_file
        .get("project")
        .and_then(Value::as_str)
        .and_then(|project| Uuid::parse_str(project).ok())
        .ok_or_else(|| {
            holds_no_project(format!(
                "its `{PROJECT_FILE_NAME}` names no project UUID under `project`"
            ))
        })?;
    let name = project_file
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Ok((project, name))
}

fn lowercase_hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
