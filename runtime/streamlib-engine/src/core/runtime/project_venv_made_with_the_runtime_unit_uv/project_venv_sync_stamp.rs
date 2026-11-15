// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The stamp naming what a project's `.venv` was last synced from, and the files and wheels it is taken over.

use std::path::{Path, PathBuf};

use super::PackagesTheRuntimeUnitCarries;

/// The file in a project's `.venv` naming what its last sync was made from.
pub(super) const PROJECT_VENV_SYNC_STAMP_FILE_NAME: &str = "tatolab-runtime-sync";

/// The files of a project whose change makes its `.venv` sync again.
const PROJECT_FILES_A_VENV_IS_SYNCED_FROM: [&str; 3] =
    ["pyproject.toml", PROJECT_LOCK_FILE_NAME, ".python-version"];

/// The project's lock file, which `uv sync` may write as it syncs.
const PROJECT_LOCK_FILE_NAME: &str = "uv.lock";

/// Whether a venv sync stamp is taken over the project's `uv.lock` too.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ProjectLockFileInTheVenvSyncStamp {
    /// The stamp a venv is recorded as synced with.
    TakenIn,
    /// Only what uv reads and never writes, compared across one sync.
    LeftOut,
}

/// What a project's `.venv` would be synced from now: the SHA-256 over its
/// `pyproject.toml`, `uv.lock` and `.python-version`, each import's
/// `pyproject.toml`, this runtime's version and the path, size and modification
/// time of each first-party module's wheel, and the name of each wheel and
/// Python archive the unit carries. `None` when the project holds no
/// `pyproject.toml`, which uv syncs nothing from.
pub(super) fn the_venv_sync_stamp_of(
    project_directory: &Path,
    packages_the_runtime_unit_carries: &PackagesTheRuntimeUnitCarries,
) -> Option<String> {
    the_venv_sync_stamp_with_the_lock_file(
        project_directory,
        packages_the_runtime_unit_carries,
        ProjectLockFileInTheVenvSyncStamp::TakenIn,
    )
}

/// The venv sync stamp of the project in `project_directory`, its `uv.lock`
/// taken in or left out as `lock_file_in_the_stamp` says.
pub(super) fn the_venv_sync_stamp_with_the_lock_file(
    project_directory: &Path,
    packages_the_runtime_unit_carries: &PackagesTheRuntimeUnitCarries,
    lock_file_in_the_stamp: ProjectLockFileInTheVenvSyncStamp,
) -> Option<String> {
    use sha2::{Digest, Sha256};
    if !project_directory.join("pyproject.toml").is_file() {
        return None;
    }
    let mut stamp = Sha256::new();
    stamp.update(env!("CARGO_PKG_VERSION").as_bytes());
    for file_name in PROJECT_FILES_A_VENV_IS_SYNCED_FROM {
        if file_name == PROJECT_LOCK_FILE_NAME
            && lock_file_in_the_stamp == ProjectLockFileInTheVenvSyncStamp::LeftOut
        {
            continue;
        }
        stamp.update(file_name.as_bytes());
        stamp.update(std::fs::read(project_directory.join(file_name)).unwrap_or_default());
    }
    for import_pyproject in each_import_pyproject_of_the_project_in(project_directory) {
        stamp.update(import_pyproject.as_os_str().as_encoded_bytes());
        stamp.update(std::fs::read(&import_pyproject).unwrap_or_default());
    }
    for carried_directory in [
        &packages_the_runtime_unit_carries.carried_wheels_directory,
        &packages_the_runtime_unit_carries.carried_python_downloads_directory,
    ]
    .into_iter()
    .flatten()
    {
        for carried_file_name in every_file_name_below(carried_directory) {
            stamp.update(carried_file_name.as_bytes());
        }
    }
    for module_wheel in &packages_the_runtime_unit_carries.first_party_module_wheels {
        stamp.update(module_wheel.as_os_str().as_encoded_bytes());
        if let Ok(wheel_metadata) = std::fs::metadata(module_wheel) {
            stamp.update(wheel_metadata.len().to_le_bytes());
            if let Ok(modified) = wheel_metadata.modified() {
                stamp.update(format!("{modified:?}").as_bytes());
            }
        }
    }
    Some(
        stamp
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

/// The path of each file below `directory`, relative to it, in name order; none when it is
/// not there.
fn every_file_name_below(directory: &Path) -> Vec<String> {
    let mut file_names = Vec::new();
    let mut directories_to_read = vec![directory.to_owned()];
    while let Some(directory_to_read) = directories_to_read.pop() {
        let Ok(entries) = std::fs::read_dir(&directory_to_read) else {
            continue;
        };
        for entry in entries.filter_map(|entry| entry.ok()) {
            let path = entry.path();
            if path.is_dir() {
                directories_to_read.push(path);
            } else if let Ok(relative) = path.strip_prefix(directory) {
                file_names.push(relative.to_string_lossy().into_owned());
            }
        }
    }
    file_names.sort();
    file_names
}

/// Each wheel file in `first_party_modules_directory`, in name order; none
/// when the directory is not there.
pub(crate) fn every_first_party_module_wheel_in(
    first_party_modules_directory: &Path,
) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(first_party_modules_directory) else {
        return Vec::new();
    };
    let mut module_wheels: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.is_file() && path.extension().is_some_and(|extension| extension == "whl")
        })
        .collect();
    module_wheels.sort();
    module_wheels
}

/// Whether `venv` holds a `.dist-info` of every module of
/// `first_party_module_wheels`, which a command outside the runtime, such as
/// `uv pip uninstall`, may have taken out.
pub(super) fn every_first_party_module_is_installed_in(
    venv: &Path,
    first_party_module_wheels: &[PathBuf],
) -> bool {
    let Ok(library_entries) = std::fs::read_dir(venv.join("lib")) else {
        return first_party_module_wheels.is_empty();
    };
    let installed_dist_info_names: Vec<String> = library_entries
        .filter_map(|entry| entry.ok())
        .filter_map(|python_directory| {
            std::fs::read_dir(python_directory.path().join("site-packages")).ok()
        })
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.ends_with(".dist-info"))
        .map(|name| name.to_ascii_lowercase())
        .collect();
    first_party_module_wheels.iter().all(|module_wheel| {
        the_dist_info_prefix_of_the_module_wheel(module_wheel).is_some_and(|prefix| {
            installed_dist_info_names
                .iter()
                .any(|installed| installed.starts_with(&prefix))
        })
    })
}

/// `<name>-` as a `.dist-info` directory spells the distribution of
/// `module_wheel`: the wheel file name's own escaped name, lowercase.
pub(super) fn the_dist_info_prefix_of_the_module_wheel(module_wheel: &Path) -> Option<String> {
    Some(format!(
        "{}-",
        the_escaped_distribution_name_of_the_module_wheel(module_wheel)?.to_ascii_lowercase()
    ))
}

/// The distribution `module_wheel` installs, from its file name: the wheel
/// spec writes the name's every `-` as `_`, and each first-party name is
/// `tatolab-<name>`, so the `-` spelling is restored.
pub(super) fn the_distribution_name_of_the_module_wheel(module_wheel: &Path) -> Option<String> {
    Some(the_escaped_distribution_name_of_the_module_wheel(module_wheel)?.replace('_', "-"))
}

/// The distribution segment of `module_wheel`'s file name, before its first `-`.
fn the_escaped_distribution_name_of_the_module_wheel(module_wheel: &Path) -> Option<String> {
    let wheel_file_name = module_wheel.file_name()?.to_str()?;
    let (escaped_distribution_name, _) = wheel_file_name.split_once('-')?;
    Some(escaped_distribution_name.to_owned())
}

/// Write into the project's `.venv` what it is synced from now, so a load
/// finding the same runs no uv.
#[cfg(test)]
pub(crate) fn record_the_venv_synced_with_what_the_project_holds_now(
    project_directory: &Path,
    packages_the_runtime_unit_carries: &PackagesTheRuntimeUnitCarries,
) -> std::io::Result<()> {
    match the_venv_sync_stamp_of(project_directory, packages_the_runtime_unit_carries) {
        Some(sync_stamp) => std::fs::write(
            project_directory
                .join(".venv")
                .join(PROJECT_VENV_SYNC_STAMP_FILE_NAME),
            sync_stamp,
        ),
        None => Ok(()),
    }
}

/// The `pyproject.toml` of each folder in the project's `imports/`, in name
/// order: uv reads each one as a path source of the project.
fn each_import_pyproject_of_the_project_in(project_directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(
        project_directory.join(streamlib_project_repository::PROJECT_IMPORTS_DIRECTORY),
    ) else {
        return Vec::new();
    };
    let mut import_pyprojects: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path().join("pyproject.toml")))
        .filter(|import_pyproject| import_pyproject.is_file())
        .collect();
    import_pyprojects.sort();
    import_pyprojects
}
