// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::project_file::PROJECT_FILE_NAME;
use crate::project_repository::ProjectRepository;
use crate::project_repository_refusal::ProjectRepositoryRefusal;

/// One project the runtime holds open, which only one caller writes at a time.
pub type ProjectRepositoryOneWriterAtATime = Arc<Mutex<ProjectRepository>>;

/// Every project this runtime has opened, one [`ProjectRepository`] per resolved directory, so
/// each project has exactly one writer however many connections address it.
#[derive(Default)]
pub struct ProjectRepositoriesOpenInThisRuntime {
    project_repository_by_resolved_directory:
        Mutex<BTreeMap<PathBuf, ProjectRepositoryOneWriterAtATime>>,
}

impl ProjectRepositoriesOpenInThisRuntime {
    /// The project at the absolute `project_directory`, opened the first time it is asked for.
    pub fn the_project_repository_at(
        &self,
        project_directory: &Path,
    ) -> Result<ProjectRepositoryOneWriterAtATime, ProjectRepositoryRefusal> {
        if !project_directory.is_absolute() {
            return Err(ProjectRepositoryRefusal::ProjectDirectoryNotAbsolute(
                project_directory.to_path_buf(),
            ));
        }
        let resolved_project_directory =
            std::fs::canonicalize(project_directory).map_err(|_| {
                ProjectRepositoryRefusal::NotAProject {
                    project_directory: project_directory.to_path_buf(),
                    project_file_name: PROJECT_FILE_NAME,
                }
            })?;
        let mut project_repository_by_resolved_directory =
            self.project_repository_by_resolved_directory.lock();
        if let Some(open) =
            project_repository_by_resolved_directory.get(&resolved_project_directory)
        {
            return Ok(Arc::clone(open));
        }
        let opened = Arc::new(Mutex::new(ProjectRepository::open(
            &resolved_project_directory,
        )?));
        project_repository_by_resolved_directory
            .insert(resolved_project_directory, Arc::clone(&opened));
        Ok(opened)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_transaction::ProjectTransactionAuthor;

    #[test]
    fn two_spellings_of_one_project_reach_the_one_writer() {
        let parent = tempfile::tempdir().unwrap();
        let project_directory = parent.path().join("arm");
        ProjectRepository::create(
            &project_directory,
            "arm",
            &ProjectTransactionAuthor::the_runtime(),
        )
        .unwrap();
        let open = ProjectRepositoriesOpenInThisRuntime::default();
        let first = open.the_project_repository_at(&project_directory).unwrap();
        let second = open
            .the_project_repository_at(&parent.path().join("arm/../arm"))
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn a_directory_with_no_project_file_is_refused_and_not_held() {
        let parent = tempfile::tempdir().unwrap();
        let open = ProjectRepositoriesOpenInThisRuntime::default();
        let refusal = open.the_project_repository_at(parent.path()).err().unwrap();
        assert!(matches!(
            refusal,
            ProjectRepositoryRefusal::NotAProject { .. }
        ));
        assert!(
            open.project_repository_by_resolved_directory
                .lock()
                .is_empty()
        );
        let refusal = open
            .the_project_repository_at(Path::new("relative/arm"))
            .err()
            .unwrap();
        assert!(matches!(
            refusal,
            ProjectRepositoryRefusal::ProjectDirectoryNotAbsolute(_)
        ));
    }
}
