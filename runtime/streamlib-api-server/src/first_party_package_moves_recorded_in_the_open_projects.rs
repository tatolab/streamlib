// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::Path;
use std::sync::Weak;

use streamlib::sdk::runtime::ProjectHistoryRecordingFirstPartyPackageMoves;
use streamlib_project_repository::{
    ProjectActionIdempotencyKey, ProjectRepositoriesOpenInThisRuntime, ProjectRepositoryRefusal,
    ProjectTransactionAuthor,
};

/// The file a move of a project's first-party packages writes.
const PROJECT_UV_LOCK_FILE_NAME: &str = "uv.lock";

/// Each move of a project's first-party packages, recorded through the one writer of the
/// project the local API holds open and announced on its change feed as every transaction is.
/// It holds the open projects weakly, since they hold the runtime that holds this.
pub(crate) struct FirstPartyPackageMovesRecordedInTheOpenProjects {
    pub(crate) project_repositories_open_in_this_runtime:
        Weak<ProjectRepositoriesOpenInThisRuntime>,
}

impl FirstPartyPackageMovesRecordedInTheOpenProjects {
    fn the_open_projects(
        &self,
    ) -> Result<std::sync::Arc<ProjectRepositoriesOpenInThisRuntime>, String> {
        self.project_repositories_open_in_this_runtime
            .upgrade()
            .ok_or_else(|| "the local API holding the project open has stopped".to_owned())
    }
}

impl ProjectHistoryRecordingFirstPartyPackageMoves
    for FirstPartyPackageMovesRecordedInTheOpenProjects
{
    fn holds_a_move_recorded_under(
        &self,
        project_directory: &Path,
        move_key: &str,
    ) -> Result<bool, String> {
        let move_key = ProjectActionIdempotencyKey::from_caller(move_key)
            .map_err(|refusal| refusal.to_string())?;
        let project_repository = self
            .the_open_projects()?
            .the_project_repository_at(project_directory)
            .map_err(|refusal| refusal.to_string())?;
        let project_repository = project_repository.lock();
        project_repository
            .the_transaction_applied_under_a_key_the_caller_sent(Some(&move_key))
            .map(|applied_before| applied_before.is_some())
            .map_err(|refusal| refusal.to_string())
    }

    fn record_the_moved_uv_lock(
        &self,
        project_directory: &Path,
        move_key: &str,
        uv_lock: &[u8],
    ) -> Result<(), String> {
        let move_key = ProjectActionIdempotencyKey::from_caller(move_key)
            .map_err(|refusal| refusal.to_string())?;
        let open_projects = self.the_open_projects()?;
        let project_repository = open_projects
            .the_project_repository_at(project_directory)
            .map_err(|refusal| refusal.to_string())?;
        let mut project_repository = project_repository.lock();
        match project_repository.write_project_file(
            &ProjectTransactionAuthor::the_runtime(),
            Some(&move_key),
            PROJECT_UV_LOCK_FILE_NAME,
            uv_lock,
            None,
        ) {
            Ok(outcome) => {
                open_projects
                    .announce_the_transaction_if_applied_now(&project_repository, &outcome);
                Ok(())
            }
            Err(ProjectRepositoryRefusal::ProjectFileAlreadyHoldsThisContent(_)) => Ok(()),
            Err(refusal) => Err(refusal.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use streamlib_project_repository::{ProjectChangeOnTheFeed, ProjectRepository};

    const MOVE_KEY: &str = "tatolab-runtime-moves-first-party-packages-to-0.68.29";

    struct AnOpenProjectLockingItsPackages {
        _root: tempfile::TempDir,
        project_directory: std::path::PathBuf,
        open_projects: Arc<ProjectRepositoriesOpenInThisRuntime>,
        recorder: FirstPartyPackageMovesRecordedInTheOpenProjects,
    }

    impl AnOpenProjectLockingItsPackages {
        fn new() -> Self {
            let root = tempfile::tempdir().expect("a temporary directory");
            let project_directory = root.path().join("desk");
            let mut repository = ProjectRepository::create(
                &project_directory,
                "desk",
                &ProjectTransactionAuthor::the_runtime(),
            )
            .expect("the project");
            repository
                .write_project_file(
                    &ProjectTransactionAuthor::the_runtime(),
                    None,
                    PROJECT_UV_LOCK_FILE_NAME,
                    b"the lock at 0.68.27\n",
                    None,
                )
                .expect("the first lock");
            drop(repository);
            let open_projects = Arc::new(ProjectRepositoriesOpenInThisRuntime::default());
            let recorder = FirstPartyPackageMovesRecordedInTheOpenProjects {
                project_repositories_open_in_this_runtime: Arc::downgrade(&open_projects),
            };
            Self {
                _root: root,
                project_directory,
                open_projects,
                recorder,
            }
        }

        fn uv_lock(&self) -> String {
            std::fs::read_to_string(self.project_directory.join(PROJECT_UV_LOCK_FILE_NAME))
                .expect("the uv.lock")
        }
    }

    #[test]
    fn a_move_is_one_transaction_by_the_runtime_announced_on_the_change_feed() {
        let project = AnOpenProjectLockingItsPackages::new();
        let mut change_feed = project.open_projects.subscribe_to_the_change_feed();
        assert!(
            !project
                .recorder
                .holds_a_move_recorded_under(&project.project_directory, MOVE_KEY)
                .expect("the history read")
        );

        project
            .recorder
            .record_the_moved_uv_lock(
                &project.project_directory,
                MOVE_KEY,
                b"the lock at 0.68.29\n",
            )
            .expect("the move recorded");

        assert_eq!(project.uv_lock(), "the lock at 0.68.29\n");
        assert!(
            project
                .recorder
                .holds_a_move_recorded_under(&project.project_directory, MOVE_KEY)
                .expect("the history read")
        );
        let ProjectChangeOnTheFeed::Applied(applied) =
            change_feed.try_recv().expect("an announcement").change
        else {
            panic!("the move was announced as applied");
        };
        assert_eq!(
            applied.transaction.author,
            ProjectTransactionAuthor::the_runtime()
        );
    }

    #[test]
    fn an_undo_by_the_runtime_puts_the_lock_back_and_the_move_stays_recorded() {
        let project = AnOpenProjectLockingItsPackages::new();
        project
            .recorder
            .record_the_moved_uv_lock(
                &project.project_directory,
                MOVE_KEY,
                b"the lock at 0.68.29\n",
            )
            .expect("the move recorded");
        let project_repository = project
            .open_projects
            .the_project_repository_at(&project.project_directory)
            .expect("the open project");
        let mut project_repository = project_repository.lock();

        let undo = project_repository
            .undo_by(&ProjectTransactionAuthor::the_runtime())
            .expect("the move is undoable");
        project_repository
            .apply_project_operations(
                &ProjectTransactionAuthor::the_runtime(),
                None,
                Vec::new(),
                undo.operations,
                undo.lineage,
            )
            .expect("the undo");
        drop(project_repository);

        assert_eq!(project.uv_lock(), "the lock at 0.68.27\n");
        assert!(
            project
                .recorder
                .holds_a_move_recorded_under(&project.project_directory, MOVE_KEY)
                .expect("the history read")
        );
    }

    #[test]
    fn a_move_once_the_local_api_stopped_is_refused_saying_so() {
        let project = AnOpenProjectLockingItsPackages::new();
        let recorder = FirstPartyPackageMovesRecordedInTheOpenProjects {
            project_repositories_open_in_this_runtime: Weak::new(),
        };

        let refusal = recorder
            .record_the_moved_uv_lock(&project.project_directory, MOVE_KEY, b"moved\n")
            .expect_err("no open projects");

        assert!(refusal.contains("the local API"), "{refusal}");
        assert_eq!(project.uv_lock(), "the lock at 0.68.27\n");
    }
}
