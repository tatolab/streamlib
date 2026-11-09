// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::project_file::PROJECT_FILE_NAME;
use crate::project_repository::ProjectRepository;
use crate::project_repository_refusal::ProjectRepositoryRefusal;
use crate::project_transaction::AppliedProjectTransaction;

/// How many announcements a slow listener may fall behind before it is told it lagged.
const PROJECT_TRANSACTION_FEED_CAPACITY: usize = 256;

/// A transaction one project's writer applied, as the change feed announces it.
#[derive(Clone, Debug)]
pub struct ProjectTransactionAnnouncement {
    /// The project's UUID, from its `tatolab-project.json`.
    pub project: Uuid,
    /// The project directory, symlinks resolved.
    pub project_directory: PathBuf,
    /// The transaction and the commit that records it.
    pub applied: AppliedProjectTransaction,
}

/// One project the runtime holds open, which only one caller writes at a time.
pub type ProjectRepositoryOneWriterAtATime = Arc<Mutex<ProjectRepository>>;

/// Every project this runtime has opened, one [`ProjectRepository`] per resolved directory, so
/// each project has exactly one writer however many connections address it, and the change
/// feed every applied transaction is announced on.
pub struct ProjectRepositoriesOpenInThisRuntime {
    project_repository_by_resolved_directory:
        Mutex<BTreeMap<PathBuf, ProjectRepositoryOneWriterAtATime>>,
    project_transaction_feed: broadcast::Sender<ProjectTransactionAnnouncement>,
}

impl Default for ProjectRepositoriesOpenInThisRuntime {
    fn default() -> Self {
        Self {
            project_repository_by_resolved_directory: Mutex::default(),
            project_transaction_feed: broadcast::channel(PROJECT_TRANSACTION_FEED_CAPACITY).0,
        }
    }
}

impl ProjectRepositoriesOpenInThisRuntime {
    /// The open project whose `tatolab-project.json` names `project`, if this runtime has opened it.
    pub fn the_open_project_repository_of(
        &self,
        project: Uuid,
    ) -> Option<ProjectRepositoryOneWriterAtATime> {
        let open_project_repositories: Vec<ProjectRepositoryOneWriterAtATime> = self
            .project_repository_by_resolved_directory
            .lock()
            .values()
            .cloned()
            .collect();
        open_project_repositories
            .into_iter()
            .find(|open| open.lock().project_file().project == project)
    }

    /// Announce `applied`, which `project_repository` just applied, on the change feed, and
    /// when the project is an import open as its own project, commit the edit on each project
    /// enclosing it, under that project's own writer.
    pub fn announce_applied_transaction(
        &self,
        project_repository: &ProjectRepository,
        applied: AppliedProjectTransaction,
    ) {
        self.commit_the_edit_on_each_enclosing_project(project_repository, &applied);
        let announcement = ProjectTransactionAnnouncement {
            project: project_repository.project_file().project,
            project_directory: project_repository.project_directory().to_path_buf(),
            applied,
        };
        if self.project_transaction_feed.send(announcement).is_err() {
            tracing::trace!("no change-feed listener is subscribed");
        }
    }

    /// Commit `applied` on each project enclosing `project_repository`, nearest first, each
    /// through its one writer, so no transaction of its own lands between its read and commit.
    fn commit_the_edit_on_each_enclosing_project(
        &self,
        project_repository: &ProjectRepository,
        applied: &AppliedProjectTransaction,
    ) {
        let mut enclosing = project_repository.the_project_enclosing_this_one();
        while let Some((enclosing_directory, folder)) = enclosing {
            let committed = self
                .the_project_repository_at(&enclosing_directory)
                .and_then(|enclosing_repository| {
                    let enclosing_repository = enclosing_repository.lock();
                    enclosing_repository
                        .commit_an_edit_of_the_import(&folder, &applied.transaction)?;
                    Ok(enclosing_repository.the_project_enclosing_this_one())
                });
            match committed {
                Ok(next) => enclosing = next,
                Err(refusal) => {
                    tracing::warn!(
                        project = %project_repository.project_directory().display(),
                        enclosing = %enclosing_directory.display(),
                        %refusal,
                        "an edit of an import did not commit on the project enclosing it"
                    );
                    return;
                }
            }
        }
    }

    /// A receiver of every transaction announced from now on, across every open project.
    pub fn subscribe_to_the_change_feed(
        &self,
    ) -> broadcast::Receiver<ProjectTransactionAnnouncement> {
        self.project_transaction_feed.subscribe()
    }

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
    fn an_announced_transaction_reaches_a_subscriber_and_is_read_back_from_history() {
        let parent = tempfile::tempdir().unwrap();
        let project_directory = parent.path().join("arm");
        ProjectRepository::create(
            &project_directory,
            "arm",
            &ProjectTransactionAuthor::the_runtime(),
        )
        .unwrap();
        std::fs::write(parent.path().join("intro.mp4"), b"video").unwrap();
        let open = ProjectRepositoriesOpenInThisRuntime::default();
        let mut change_feed = open.subscribe_to_the_change_feed();
        let project_repository = open.the_project_repository_at(&project_directory).unwrap();
        let mut project_repository = project_repository.lock();
        let applied = project_repository
            .apply_asset_changes(
                &ProjectTransactionAuthor::the_runtime(),
                vec![crate::ProjectAssetChangeRequest::ImportAsset {
                    source: crate::ProjectAssetImportSource::FileOnThisMachine(
                        parent.path().join("intro.mp4"),
                    ),
                    asset_path: "media/intro.mp4".to_owned(),
                    kind: None,
                    import_settings: serde_json::Map::new(),
                }],
            )
            .unwrap();
        open.announce_applied_transaction(&project_repository, applied.clone());
        let announced = change_feed.try_recv().unwrap();
        assert_eq!(announced.applied, applied);
        assert_eq!(announced.project, project_repository.project_file().project);
        assert_eq!(
            project_repository
                .applied_transactions_newest_first(8)
                .unwrap(),
            vec![applied]
        );
        let project = project_repository.project_file().project;
        drop(project_repository);
        assert!(open.the_open_project_repository_of(project).is_some());
        assert!(
            open.the_open_project_repository_of(Uuid::new_v4())
                .is_none()
        );
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
