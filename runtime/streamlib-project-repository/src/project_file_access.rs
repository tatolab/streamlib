// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::PROJECT_RUNTIME_DIRECTORY;
#[cfg(feature = "history")]
use crate::core_project_import::{CORE_PROJECT_IMPORT_FOLDER, the_core_project_link_in};
use crate::project_asset::DIRECTORIES_A_PROJECT_SCAN_SKIPS;
#[cfg(feature = "history")]
use crate::project_asset::PROJECT_IMPORTS_DIRECTORY;
use crate::project_repository_refusal::ProjectRepositoryRefusal;

/// A file's path inside its project as the file verbs name it: relative, `/`-separated, never
/// outside the project and never into `.tatolab/`, what the runtime keeps.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProjectRelativeFilePath(String);

impl ProjectRelativeFilePath {
    /// Check `path` names a file inside the project and outside `.tatolab/`.
    pub fn parse(path: &str) -> Result<Self, ProjectRepositoryRefusal> {
        let refuse = |why: &'static str| ProjectRepositoryRefusal::ProjectFilePathRefused {
            path: path.to_owned(),
            why,
        };
        if path.is_empty() {
            return Err(refuse("it is empty"));
        }
        if path.contains('\\') {
            return Err(refuse("its separator is `/`, never `\\`"));
        }
        if Path::new(path).is_absolute() || path.starts_with('/') {
            return Err(refuse(
                "it is absolute; a file path is relative to its project",
            ));
        }
        if path.ends_with('/') {
            return Err(refuse("it names a directory, not a file"));
        }
        if path.split('/').any(str::is_empty) {
            return Err(refuse("it holds an empty segment"));
        }
        if Path::new(path)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(refuse(
                "it holds `.`, `..` or a root, which could lead outside the project",
            ));
        }
        if path.split('/').next().is_some_and(|first_segment| {
            first_segment.eq_ignore_ascii_case(PROJECT_RUNTIME_DIRECTORY)
        }) {
            return Err(refuse("it is inside `.tatolab/`, which the runtime keeps"));
        }
        Ok(Self(path.to_owned()))
    }

    /// The path as the project spells it.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The file on disk under `project_directory`.
    pub fn on_disk_under(&self, project_directory: &Path) -> PathBuf {
        project_directory.join(&self.0)
    }

    /// Each folder the path passes through, outermost first, without the file's own name.
    pub(crate) fn folders(&self) -> impl Iterator<Item = &str> {
        let folders = self.0.rsplit_once('/').map_or("", |(folders, _)| folders);
        folders.split('/').filter(|folder| !folder.is_empty())
    }

    /// Refuse the path when a folder on its way, or the file itself, is a symbolic link, which
    /// could lead outside the project.
    pub(crate) fn refuse_a_symbolic_link_on_the_way_under(
        &self,
        project_directory: &Path,
    ) -> Result<(), ProjectRepositoryRefusal> {
        let mut on_the_way = project_directory.to_path_buf();
        for segment in self.0.split('/') {
            on_the_way.push(segment);
            if std::fs::symlink_metadata(&on_the_way).is_ok_and(|metadata| metadata.is_symlink()) {
                return Err(ProjectRepositoryRefusal::ProjectFilePathRefused {
                    path: self.0.clone(),
                    why: "it passes through a symbolic link, which could lead outside the project",
                });
            }
        }
        Ok(())
    }

    /// Refuse a read of the path when a folder on its way, or the file itself, is a symbolic
    /// link, save the one link the runtime makes to the core project it carries: a path into
    /// that import passes through the link while it points at
    /// `core_project_directory_the_runtime_carries`, and a link below it, or `imports` being
    /// one, is refused as any other.
    #[cfg(feature = "history")]
    pub(crate) fn refuse_a_symbolic_link_on_the_way_to_a_read_under(
        &self,
        project_directory: &Path,
        core_project_directory_the_runtime_carries: Option<&Path>,
    ) -> Result<(), ProjectRepositoryRefusal> {
        let the_link_points_at_the_runtimes_core_project = || {
            let imports_is_a_link =
                std::fs::symlink_metadata(project_directory.join(PROJECT_IMPORTS_DIRECTORY))
                    .is_ok_and(|metadata| metadata.is_symlink());
            !imports_is_a_link
                && core_project_directory_the_runtime_carries.is_some_and(|carried| {
                    std::fs::read_link(the_core_project_link_in(project_directory))
                        .is_ok_and(|pointed_at| pointed_at == carried)
                })
        };
        let into_the_core_project = self
            .0
            .strip_prefix(PROJECT_IMPORTS_DIRECTORY)
            .and_then(|below_imports| below_imports.strip_prefix('/'))
            .and_then(|below_imports| below_imports.strip_prefix(CORE_PROJECT_IMPORT_FOLDER))
            .and_then(|below_the_link| below_the_link.strip_prefix('/'))
            .filter(|_| the_link_points_at_the_runtimes_core_project());
        match into_the_core_project {
            Some(inside_the_core_project) => Self(inside_the_core_project.to_owned())
                .refuse_a_symbolic_link_on_the_way_under(&the_core_project_link_in(
                    project_directory,
                ))
                .map_err(|_| ProjectRepositoryRefusal::ProjectFilePathRefused {
                    path: self.0.clone(),
                    why: "it passes through a symbolic link, which could lead outside the project",
                }),
            None => self.refuse_a_symbolic_link_on_the_way_under(project_directory),
        }
    }
}

impl TryFrom<String> for ProjectRelativeFilePath {
    type Error = ProjectRepositoryRefusal;
    fn try_from(path: String) -> Result<Self, Self::Error> {
        Self::parse(&path)
    }
}

impl From<ProjectRelativeFilePath> for String {
    fn from(path: ProjectRelativeFilePath) -> Self {
        path.0
    }
}

impl std::fmt::Display for ProjectRelativeFilePath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One file of a project as a listing shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectFileListed {
    /// Where the file sits in the project, `/`-separated.
    pub path: String,
    /// The file's length in bytes.
    pub byte_len: u64,
}

/// Every regular file of the project at `project_directory` under `under` (the whole project
/// when `None`), sorted by path: past hidden folders, the folders a project scan skips, Python
/// virtual environments and symbolic links.
pub fn project_files_a_listing_shows(
    project_directory: &Path,
    under: Option<&ProjectRelativeFilePath>,
) -> Result<Vec<ProjectFileListed>, ProjectRepositoryRefusal> {
    let (start, start_relative) = match under {
        Some(under) => {
            under.refuse_a_symbolic_link_on_the_way_under(project_directory)?;
            let start = under.on_disk_under(project_directory);
            if !start.is_dir() {
                return Err(ProjectRepositoryRefusal::NoSuchProjectFolder(
                    under.to_string(),
                ));
            }
            (start, under.as_str().to_owned())
        }
        None => (project_directory.to_path_buf(), String::new()),
    };
    let mut listed = Vec::new();
    let mut directories_to_read = vec![(start, start_relative)];
    while let Some((directory, relative_directory)) = directories_to_read.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if relative_directory.is_empty() {
                name.clone()
            } else {
                format!("{relative_directory}/{name}")
            };
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                let is_a_listed_folder = !name.starts_with('.')
                    && !DIRECTORIES_A_PROJECT_SCAN_SKIPS.contains(&name.as_str())
                    && !entry.path().join("pyvenv.cfg").exists();
                if is_a_listed_folder {
                    directories_to_read.push((entry.path(), relative));
                }
            } else if file_type.is_file() {
                listed.push(ProjectFileListed {
                    path: relative,
                    byte_len: entry.metadata()?.len(),
                });
            }
        }
    }
    listed.sort_by(|first, second| first.path.cmp(&second.path));
    Ok(listed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_outside_the_project_or_into_what_the_runtime_keeps_is_refused() {
        for refused in [
            "",
            "/etc/passwd",
            "../outside.py",
            "nodes/../../outside.py",
            "./nodes/camera.py",
            "nodes//camera.py",
            "nodes\\camera.py",
            "nodes/",
            ".tatolab/history.git/config",
            ".tatolab",
        ] {
            assert!(
                matches!(
                    ProjectRelativeFilePath::parse(refused),
                    Err(ProjectRepositoryRefusal::ProjectFilePathRefused { .. })
                ),
                "{refused:?}"
            );
        }
        for accepted in [
            "nodes/camera.py",
            ".python-version",
            "views/panel/dist/logo.png",
        ] {
            assert_eq!(
                ProjectRelativeFilePath::parse(accepted).unwrap().as_str(),
                accepted
            );
        }
    }

    #[test]
    fn a_listing_shows_every_file_but_hidden_folders_venvs_and_what_a_scan_skips() {
        let project_directory = tempfile::tempdir().unwrap();
        for path in [
            "stream.py",
            ".python-version",
            "nodes/camera.py",
            "views/panel/dist/logo.png",
            ".tatolab/history.git/HEAD",
            ".venv/bin/python",
            "env/pyvenv.cfg",
            "nodes/__pycache__/camera.pyc",
            "views/panel/node_modules/left-pad/index.js",
        ] {
            let on_disk = project_directory.path().join(path);
            std::fs::create_dir_all(on_disk.parent().unwrap()).unwrap();
            std::fs::write(on_disk, path).unwrap();
        }
        let listed_paths = |under: Option<&str>| -> Vec<String> {
            let under = under.map(|under| ProjectRelativeFilePath::parse(under).unwrap());
            project_files_a_listing_shows(project_directory.path(), under.as_ref())
                .unwrap()
                .into_iter()
                .map(|listed| listed.path)
                .collect()
        };

        assert_eq!(
            listed_paths(None),
            [
                ".python-version",
                "nodes/camera.py",
                "stream.py",
                "views/panel/dist/logo.png"
            ]
        );
        assert_eq!(listed_paths(Some("views")), ["views/panel/dist/logo.png"]);
        assert!(matches!(
            project_files_a_listing_shows(
                project_directory.path(),
                Some(&ProjectRelativeFilePath::parse("missing").unwrap())
            ),
            Err(ProjectRepositoryRefusal::NoSuchProjectFolder(_))
        ));
    }
}
