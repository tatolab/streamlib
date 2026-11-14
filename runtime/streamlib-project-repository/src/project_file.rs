// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::canonical_project_json::canonical_project_json_bytes;
use crate::project_repository_refusal::ProjectRepositoryRefusal;

/// The project file's name at the root of every project.
pub const PROJECT_FILE_NAME: &str = "tatolab-project.json";

/// What `tatolab-project.json` holds: the project's identity, how people see it, and the modules
/// it imports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TatolabProjectFile {
    /// The project's UUID, generated when the project was created and never changed.
    pub project: Uuid,
    /// The project's name as people see it; a label, never an identity.
    pub name: String,
    /// What the project is, in the words of whoever described it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The image asset the project is shown by, by its UUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<Uuid>,
    /// The version of the runtime that created the project: a record, never a lock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_with_runtime: Option<String>,
    /// Each project copied into this one's `imports/` folder.
    #[serde(default)]
    pub dependencies: Vec<ProjectImport>,
}

/// Another project copied whole into `imports/<folder>/` and referenced there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectImport {
    /// The imported project's UUID.
    pub project: Uuid,
    /// The folder under `imports/` that holds it.
    pub folder: String,
    /// Where it was copied from, as `add_dependency` was given it.
    pub source: String,
    /// The SHA-256 of the files the copy carried, which names the version it carried.
    pub version: String,
    /// The SHA-256 of each file the copy carried, by its path inside the folder.
    pub copied_files: BTreeMap<String, String>,
}

/// The version of this runtime, as a new project records it.
pub const THIS_RUNTIME_VERSION: &str = env!("CARGO_PKG_VERSION");

impl TatolabProjectFile {
    /// A new project's file, with a fresh UUID, this runtime's version and no imports.
    pub fn of_a_new_project(name: &str) -> Self {
        Self {
            project: Uuid::new_v4(),
            name: name.to_owned(),
            description: None,
            cover: None,
            created_with_runtime: Some(THIS_RUNTIME_VERSION.to_owned()),
            dependencies: Vec::new(),
        }
    }

    /// The import of `project`, when this project holds one.
    pub fn import_of(&self, project: Uuid) -> Option<&ProjectImport> {
        self.dependencies
            .iter()
            .find(|import| import.project == project)
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
