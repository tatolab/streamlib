// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::{Path, PathBuf};

use crate::project_file::PROJECT_FILE_NAME;
use crate::project_history_repository::{PROJECT_HISTORY_DIRECTORY, ProjectHistoryRemote};
use crate::project_repository_refusal::ProjectRepositoryRefusal;

/// How a dependency source names a Python package installed in the depending project's venv:
/// `package:<distribution name>`.
pub const PYTHON_PACKAGE_DEPENDENCY_SOURCE_PREFIX: &str = "package:";

/// Where the depending project's venv keeps installed packages, relative to the project.
const PROJECT_VENV_LIB_DIRECTORY: &str = ".venv/lib";

/// Where a dependency's history is fetched from, as `tatolab-project.json` records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectDependencySource {
    /// A project directory on this machine, fetched from its own history.
    ProjectDirectory(PathBuf),
    /// A Python distribution installed in the depending project's venv whose files hold one
    /// project directory with its history.
    PythonPackageShippingAProject { distribution: String },
    /// A Git smart-HTTP remote, such as an Artifacts repo, by its URL.
    GitRemote(String),
}

impl ProjectDependencySource {
    /// The source `source` names, as `add_dependency` is given it.
    pub fn named_by(source: &str) -> Self {
        if let Some(distribution) = source.strip_prefix(PYTHON_PACKAGE_DEPENDENCY_SOURCE_PREFIX) {
            return Self::PythonPackageShippingAProject {
                distribution: distribution.trim().to_owned(),
            };
        }
        let source_path = Path::new(source);
        if source_path.join(PROJECT_FILE_NAME).is_file() {
            return Self::ProjectDirectory(source_path.to_path_buf());
        }
        Self::GitRemote(source.to_owned())
    }

    /// The remote this source is fetched from by the project at `depending_project_directory`;
    /// `bearer_token` is sent to a Git remote alone, never to a directory on this machine.
    pub fn the_remote_to_fetch_from(
        &self,
        depending_project_directory: &Path,
        bearer_token: Option<&str>,
    ) -> Result<ProjectHistoryRemote, ProjectRepositoryRefusal> {
        let project_history_remote = |project_directory: &Path| ProjectHistoryRemote {
            url: project_directory
                .join(PROJECT_HISTORY_DIRECTORY)
                .display()
                .to_string(),
            bearer_token: None,
        };
        match self {
            Self::ProjectDirectory(project_directory) => {
                Ok(project_history_remote(project_directory))
            }
            Self::PythonPackageShippingAProject { distribution } => Ok(project_history_remote(
                &the_project_directory_an_installed_package_ships(
                    depending_project_directory,
                    distribution,
                )?,
            )),
            Self::GitRemote(url) => Ok(ProjectHistoryRemote {
                url: url.clone(),
                bearer_token: bearer_token.map(str::to_owned),
            }),
        }
    }
}

/// The project directory the distribution `distribution`, installed in the venv of the project
/// at `depending_project_directory`, ships: the one folder among its installed files holding
/// both `tatolab-project.json` and the project's history.
pub fn the_project_directory_an_installed_package_ships(
    depending_project_directory: &Path,
    distribution: &str,
) -> Result<PathBuf, ProjectRepositoryRefusal> {
    let venv_lib_directory = depending_project_directory.join(PROJECT_VENV_LIB_DIRECTORY);
    let wanted_name = normalized_python_distribution_name(distribution);
    let not_installed = || ProjectRepositoryRefusal::DependencyPackageNotInstalled {
        distribution: distribution.to_owned(),
        venv_lib_directory: venv_lib_directory.clone(),
    };
    let mut site_packages_directories = Vec::new();
    for python_lib_entry in std::fs::read_dir(&venv_lib_directory).map_err(|_| not_installed())? {
        let site_packages_directory = python_lib_entry?.path().join("site-packages");
        if site_packages_directory.is_dir() {
            site_packages_directories.push(site_packages_directory);
        }
    }
    site_packages_directories.sort();
    for site_packages_directory in site_packages_directories {
        let mut dist_info_directories: Vec<PathBuf> = std::fs::read_dir(&site_packages_directory)?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.extension().and_then(|extension| extension.to_str()) == Some("dist-info")
            })
            .collect();
        dist_info_directories.sort();
        for dist_info_directory in dist_info_directories {
            let Ok(metadata) = std::fs::read_to_string(dist_info_directory.join("METADATA")) else {
                continue;
            };
            let names_the_distribution = metadata
                .lines()
                .take_while(|line| !line.is_empty())
                .filter_map(|line| line.strip_prefix("Name:"))
                .any(|name| normalized_python_distribution_name(name.trim()) == wanted_name);
            if !names_the_distribution {
                continue;
            }
            return the_one_project_directory_among_installed_files(
                &site_packages_directory,
                &dist_info_directory,
                distribution,
            );
        }
    }
    Err(not_installed())
}

fn the_one_project_directory_among_installed_files(
    site_packages_directory: &Path,
    dist_info_directory: &Path,
    distribution: &str,
) -> Result<PathBuf, ProjectRepositoryRefusal> {
    let installed_files = std::fs::read_to_string(dist_info_directory.join("RECORD"))?;
    let mut project_directories: Vec<PathBuf> = installed_files
        .lines()
        .filter_map(|record_line| record_line.split(',').next())
        .filter(|installed_path| {
            Path::new(installed_path)
                .file_name()
                .and_then(|name| name.to_str())
                == Some(PROJECT_FILE_NAME)
        })
        .filter_map(|installed_path| {
            site_packages_directory
                .join(installed_path)
                .parent()
                .map(Path::to_path_buf)
        })
        .filter(|project_directory| project_directory.join(PROJECT_HISTORY_DIRECTORY).is_dir())
        .collect();
    project_directories.sort();
    project_directories.dedup();
    match project_directories.len() {
        1 => Ok(project_directories.remove(0)),
        0 => Err(ProjectRepositoryRefusal::DependencyPackageShipsNoProject {
            distribution: distribution.to_owned(),
        }),
        _ => Err(
            ProjectRepositoryRefusal::DependencyPackageShipsSeveralProjects {
                distribution: distribution.to_owned(),
                project_directories,
            },
        ),
    }
}

/// A distribution name as PEP 503 compares it: lowercase, each run of `-`, `_` and `.` one `-`.
fn normalized_python_distribution_name(name: &str) -> String {
    let mut normalized = String::with_capacity(name.len());
    let mut in_a_separator_run = false;
    for character in name.chars() {
        if matches!(character, '-' | '_' | '.') {
            if !in_a_separator_run {
                normalized.push('-');
            }
            in_a_separator_run = true;
        } else {
            normalized.extend(character.to_lowercase());
            in_a_separator_run = false;
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distribution_names_compare_as_pep_503_normalizes_them() {
        assert_eq!(
            normalized_python_distribution_name("Tatolab_Robot..Arm"),
            "tatolab-robot-arm"
        );
    }

    #[test]
    fn a_token_is_sent_to_a_git_remote_and_never_to_a_directory() {
        let project_directory = tempfile::tempdir().unwrap();
        std::fs::write(project_directory.path().join(PROJECT_FILE_NAME), b"{}").unwrap();
        let directory_source =
            ProjectDependencySource::named_by(project_directory.path().to_str().unwrap());
        assert!(matches!(
            directory_source,
            ProjectDependencySource::ProjectDirectory(_)
        ));
        let directory_remote = directory_source
            .the_remote_to_fetch_from(project_directory.path(), Some("secret"))
            .unwrap();
        assert_eq!(directory_remote.bearer_token, None);

        let git_remote = ProjectDependencySource::named_by("https://example.invalid/repo.git")
            .the_remote_to_fetch_from(project_directory.path(), Some("secret"))
            .unwrap();
        assert_eq!(git_remote.bearer_token.as_deref(), Some("secret"));
    }
}
