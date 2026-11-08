// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::canonical_project_json::canonical_project_json_bytes;
use crate::project_repository_refusal::ProjectRepositoryRefusal;

/// The project file's name at the root of every project.
pub const PROJECT_FILE_NAME: &str = "tatolab-project.json";

/// What `tatolab-project.json` holds: the project's identity and the projects it depends on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TatolabProjectFile {
    /// The project's UUID, generated when the project was created and never changed.
    pub project: Uuid,
    /// The project's name as people see it; a label, never an identity.
    pub name: String,
    /// Each project this one uses assets from, pinned to a revision.
    #[serde(default)]
    pub dependencies: Vec<PinnedProjectDependency>,
}

/// Another project this one uses, at the revision it is pinned to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedProjectDependency {
    /// The depended-on project's UUID.
    pub project: Uuid,
    /// Where its history is fetched from: a local path, an Artifacts remote or a package.
    pub source: String,
    /// The commit or tag it is pinned to.
    pub revision: String,
}

impl TatolabProjectFile {
    /// A new project's file, with a fresh UUID and no dependencies.
    pub fn of_a_new_project(name: &str) -> Self {
        Self {
            project: Uuid::new_v4(),
            name: name.to_owned(),
            dependencies: Vec::new(),
        }
    }

    /// Read the project file at the root of `project_directory`.
    pub fn read_from_project_directory(
        project_directory: &Path,
    ) -> Result<Self, ProjectRepositoryRefusal> {
        let path = project_directory.join(PROJECT_FILE_NAME);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(ProjectRepositoryRefusal::NotAProject {
                    project_directory: project_directory.to_path_buf(),
                    project_file_name: PROJECT_FILE_NAME,
                });
            }
            Err(error) => return Err(error.into()),
        };
        serde_json::from_slice(&bytes).map_err(|error| {
            ProjectRepositoryRefusal::ProjectFileUnreadable {
                path,
                why: error.to_string(),
            }
        })
    }

    /// The file's canonical bytes, as the runtime writes and history records them.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ProjectRepositoryRefusal> {
        Ok(canonical_project_json_bytes(self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_file_reads_back_what_it_wrote() {
        let project_directory = tempfile::tempdir().unwrap();
        let written = TatolabProjectFile::of_a_new_project("robot-cell");
        std::fs::write(
            project_directory.path().join(PROJECT_FILE_NAME),
            written.canonical_bytes().unwrap(),
        )
        .unwrap();
        assert_eq!(
            TatolabProjectFile::read_from_project_directory(project_directory.path()).unwrap(),
            written
        );
    }

    #[test]
    fn a_directory_without_a_project_file_is_not_a_project() {
        let project_directory = tempfile::tempdir().unwrap();
        assert!(matches!(
            TatolabProjectFile::read_from_project_directory(project_directory.path()),
            Err(ProjectRepositoryRefusal::NotAProject { .. })
        ));
    }

    #[test]
    fn an_unknown_key_is_refused_by_name() {
        let project_directory = tempfile::tempdir().unwrap();
        std::fs::write(
            project_directory.path().join(PROJECT_FILE_NAME),
            format!(
                "{{\"project\": \"{}\", \"name\": \"a\", \"colour\": \"red\"}}",
                Uuid::new_v4()
            ),
        )
        .unwrap();
        let refusal =
            TatolabProjectFile::read_from_project_directory(project_directory.path()).unwrap_err();
        assert!(refusal.to_string().contains("colour"), "{refusal}");
    }
}
