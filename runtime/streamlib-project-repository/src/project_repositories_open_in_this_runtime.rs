// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::project_file::PROJECT_FILE_NAME;
use crate::project_files_changed_outside_the_runtime_watch::ProjectFilesChangedOutsideTheRuntimeWatch;
use crate::project_repository::{ProjectPythonRequirementsLock, ProjectRepository};
use crate::project_repository_refusal::ProjectRepositoryRefusal;
use crate::project_transaction::{AppliedProjectTransaction, ProjectActionApplyOutcome};

/// How many announcements a slow listener may fall behind before it is told it lagged.
const PROJECT_CHANGE_FEED_CAPACITY: usize = 256;

/// A change to one project, as the change feed announces it.
#[derive(Clone, Debug)]
pub struct ProjectChangeFeedAnnouncement {
    /// The project's UUID, from its `tatolab-project.json`.
    pub project: Uuid,
    /// What changed on the project's feed.
    pub change: ProjectChangeOnTheFeed,
}

/// What one announcement on the change feed tells of.
#[derive(Clone, Debug, PartialEq)]
pub enum ProjectChangeOnTheFeed {
    /// The project's writer applied this transaction, which its history now records.
    Applied(Box<AppliedProjectTransaction>),
    /// The runtime refused an action on the project, which no history records.
    Refused,
    /// The runtime finished reading an asset's media facts, which no history records.
    MediaFactsRead,
    /// Files in the project's folder changed, appeared or went away, whoever wrote them, each by
    /// its path relative to the project; a transaction's own writes are among them.
    FilesChanged(Vec<String>),
}

/// A hold on the watch of one open project's folder; the folder is watched while any hold lives.
pub struct ProjectFilesWatchedWhileHeld {
    _watch_kept_alive: Arc<ProjectFilesChangedOutsideTheRuntimeWatch>,
}

impl std::fmt::Debug for ProjectFilesWatchedWhileHeld {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ProjectFilesWatchedWhileHeld")
    }
}

/// One project the runtime holds open, which only one caller writes at a time.
pub type ProjectRepositoryOneWriterAtATime = Arc<Mutex<ProjectRepository>>;

/// Every project this runtime has opened, one [`ProjectRepository`] per resolved directory, so
/// each project has exactly one writer however many connections address it, and the change
/// feed every applied transaction is announced on.
pub struct ProjectRepositoriesOpenInThisRuntime {
    project_repository_by_resolved_directory:
        Mutex<BTreeMap<PathBuf, ProjectRepositoryOneWriterAtATime>>,
    project_files_watch_by_project:
        Mutex<BTreeMap<Uuid, Weak<ProjectFilesChangedOutsideTheRuntimeWatch>>>,
    project_change_feed: broadcast::Sender<ProjectChangeFeedAnnouncement>,
    python_requirements_lock: Option<ProjectPythonRequirementsLock>,
}

impl Default for ProjectRepositoriesOpenInThisRuntime {
    fn default() -> Self {
        Self {
            project_repository_by_resolved_directory: Mutex::default(),
            project_files_watch_by_project: Mutex::default(),
            project_change_feed: broadcast::channel(PROJECT_CHANGE_FEED_CAPACITY).0,
            python_requirements_lock: None,
        }
    }
}

impl ProjectRepositoriesOpenInThisRuntime {
    /// No project open yet, each one opened handed `python_requirements_lock` to lock its
    /// Python requirements with.
    pub fn locking_python_requirements_with(
        python_requirements_lock: ProjectPythonRequirementsLock,
    ) -> Self {
        Self {
            python_requirements_lock: Some(python_requirements_lock),
            ..Self::default()
        }
    }

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
        self.announce_on_the_change_feed(ProjectChangeFeedAnnouncement {
            project: project_repository.project_file().project,
            change: ProjectChangeOnTheFeed::Applied(Box::new(applied)),
        });
    }

    /// Announce that the runtime refused an action on the project `project`.
    pub fn announce_a_refused_action(&self, project: Uuid) {
        self.announce_on_the_change_feed(ProjectChangeFeedAnnouncement {
            project,
            change: ProjectChangeOnTheFeed::Refused,
        });
    }

    /// Announce that the runtime finished reading the media facts of an asset of `project`.
    pub fn announce_media_facts_read(&self, project: Uuid) {
        self.announce_on_the_change_feed(ProjectChangeFeedAnnouncement {
            project,
            change: ProjectChangeOnTheFeed::MediaFactsRead,
        });
    }

    fn announce_on_the_change_feed(&self, announcement: ProjectChangeFeedAnnouncement) {
        if self.project_change_feed.send(announcement).is_err() {
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

    /// Announce `outcome`'s transaction as [`Self::announce_applied_transaction`] does when this
    /// call applied it; one an earlier call applied under its key was announced by that call.
    pub fn announce_the_transaction_if_applied_now(
        &self,
        project_repository: &ProjectRepository,
        outcome: &ProjectActionApplyOutcome,
    ) {
        if let ProjectActionApplyOutcome::AppliedNow(applied) = outcome {
            self.announce_applied_transaction(project_repository, applied.clone());
        }
    }

    /// A receiver of every change announced from now on, across every project.
    pub fn subscribe_to_the_change_feed(
        &self,
    ) -> broadcast::Receiver<ProjectChangeFeedAnnouncement> {
        self.project_change_feed.subscribe()
    }

    /// Close the project at `project_directory` if this runtime holds it open, so the next call
    /// naming the folder opens what is there then.
    pub fn close_the_project_repository_at(&self, project_directory: &Path) {
        let resolved_project_directory = std::fs::canonicalize(project_directory)
            .unwrap_or_else(|_| project_directory.to_path_buf());
        self.project_repository_by_resolved_directory
            .lock()
            .remove(&resolved_project_directory);
    }

    /// Watch the folder of the open project `project`, announcing each change to its files on the
    /// change feed while any returned hold lives; none when the project is not open here, or when
    /// the OS will not watch its folder, which is logged.
    pub fn watch_the_files_of(&self, project: Uuid) -> Option<ProjectFilesWatchedWhileHeld> {
        let mut project_files_watch_by_project = self.project_files_watch_by_project.lock();
        if let Some(watch) = project_files_watch_by_project
            .get(&project)
            .and_then(Weak::upgrade)
        {
            return Some(ProjectFilesWatchedWhileHeld {
                _watch_kept_alive: watch,
            });
        }
        let resolved_project_directory = self
            .the_open_project_repository_of(project)?
            .lock()
            .project_directory()
            .to_path_buf();
        let project_change_feed = self.project_change_feed.clone();
        let watched = ProjectFilesChangedOutsideTheRuntimeWatch::of_the_project_in(
            &resolved_project_directory,
            move |files_changed| {
                let _ = project_change_feed.send(ProjectChangeFeedAnnouncement {
                    project,
                    change: ProjectChangeOnTheFeed::FilesChanged(files_changed),
                });
            },
        );
        match watched {
            Ok(watch) => {
                let watch = Arc::new(watch);
                project_files_watch_by_project.retain(|_, held| held.strong_count() > 0);
                project_files_watch_by_project.insert(project, Arc::downgrade(&watch));
                Some(ProjectFilesWatchedWhileHeld {
                    _watch_kept_alive: watch,
                })
            }
            Err(not_watched) => {
                tracing::warn!(
                    project = %resolved_project_directory.display(),
                    %not_watched,
                    "the project's folder is not watched, so a change made outside the runtime is not announced"
                );
                None
            }
        }
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
        let mut opened = ProjectRepository::open(&resolved_project_directory)?;
        if let Some(python_requirements_lock) = &self.python_requirements_lock {
            opened.lock_the_python_requirements_with(Arc::clone(python_requirements_lock));
        }
        let opened = Arc::new(Mutex::new(opened));
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
                None,
                vec![crate::ProjectAssetAction::ImportAsset {
                    source: crate::ProjectAssetImportSource::FileOnThisMachine(
                        parent.path().join("intro.mp4"),
                    ),
                    asset_path: Some("media/intro.mp4".to_owned()),
                    file_name: None,
                    kind: None,
                    import_settings: serde_json::Map::new(),
                }],
            )
            .unwrap()
            .into_applied_transaction();
        open.announce_applied_transaction(&project_repository, applied.clone());
        let announced = change_feed.try_recv().unwrap();
        assert_eq!(
            announced.change,
            ProjectChangeOnTheFeed::Applied(Box::new(applied.clone()))
        );
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

    /// A node file an editor saves in an open project's folder is announced as `FilesChanged`
    /// while a hold on its watch lives, and the folder is watched no longer once every hold is
    /// dropped.
    ///
    /// Mental-revert: without the watch nothing is announced; a watch kept past its last hold
    /// leaves the map holding a live watch.
    #[test]
    fn a_file_saved_in_an_open_projects_folder_is_announced_while_its_watch_is_held() {
        let parent = tempfile::tempdir().unwrap();
        let project_directory = parent.path().join("arm");
        ProjectRepository::create(
            &project_directory,
            "arm",
            &ProjectTransactionAuthor::the_runtime(),
        )
        .unwrap();
        let open = ProjectRepositoriesOpenInThisRuntime::default();
        assert!(open.watch_the_files_of(Uuid::new_v4()).is_none());
        let project_repository = open.the_project_repository_at(&project_directory).unwrap();
        let project = project_repository.lock().project_file().project;
        let mut change_feed = open.subscribe_to_the_change_feed();
        let first_hold = open
            .watch_the_files_of(project)
            .expect("the open project is watched");
        let second_hold = open
            .watch_the_files_of(project)
            .expect("the watch is shared");

        std::fs::write(project_directory.join("blur.py"), b"x = 1").unwrap();
        let announced = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(5), change_feed.recv()).await
            })
            .expect("the save is announced")
            .unwrap();
        assert_eq!(announced.project, project);
        assert_eq!(
            announced.change,
            ProjectChangeOnTheFeed::FilesChanged(vec!["blur.py".to_owned()])
        );

        drop(first_hold);
        let is_watched = |open: &ProjectRepositoriesOpenInThisRuntime| {
            open.project_files_watch_by_project
                .lock()
                .get(&project)
                .is_some_and(|watch| watch.strong_count() > 0)
        };
        assert!(is_watched(&open));
        drop(second_hold);
        assert!(!is_watched(&open));
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
