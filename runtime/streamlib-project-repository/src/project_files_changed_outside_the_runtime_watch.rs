// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The watch on an open project's folder that tells of each file that changes, appears or goes
//! away there, whoever wrote it, coalesced so a burst of writes is told once.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{EventKind, RecursiveMode, Watcher};

use crate::project_asset::DIRECTORIES_A_PROJECT_SCAN_SKIPS;

/// How long the watch gathers the changes after the first of a burst before it tells of them.
pub(crate) const PROJECT_FILE_CHANGES_GATHERED_FOR: Duration = Duration::from_millis(100);

/// What the watch tells of one gathered burst of changes in the project's folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProjectFilesToldChanged {
    /// These files changed, appeared or went away, each by its path relative to the project, `/`
    /// between segments.
    These(Vec<String>),
    /// Files changed that the OS did not name: its queue overflowed or a folder could not be
    /// watched, so a client lists the project again.
    SomeTheOsDidNotName,
}

/// What the OS's watch hands the gatherer: an event, or word that some change went unnamed.
enum FolderWatchReport {
    Event(notify::Event),
    ChangesTheOsDidNotName,
}

/// The OS's watch on one project's folder; dropping it ends the watch.
pub(crate) struct ProjectFilesChangedOutsideTheRuntimeWatch {
    _folder_watcher: notify::RecommendedWatcher,
}

impl ProjectFilesChangedOutsideTheRuntimeWatch {
    /// Watch `resolved_project_directory`, handing `tell_of_the_files_changed` each gathered burst
    /// of changes, leaving out what the project's history records nothing of: a hidden folder,
    /// `node_modules`, `__pycache__` and a Python virtual environment.
    pub(crate) fn of_the_project_in(
        resolved_project_directory: &Path,
        tell_of_the_files_changed: impl Fn(ProjectFilesToldChanged) + Send + 'static,
    ) -> notify::Result<Self> {
        let (folder_reports_to_the_gatherer, folder_reports) = mpsc::channel::<FolderWatchReport>();
        let mut folder_watcher =
            notify::recommended_watcher(move |folder_event: notify::Result<notify::Event>| {
                let folder_report = match folder_event {
                    Ok(folder_event) if !folder_event.need_rescan() => {
                        FolderWatchReport::Event(folder_event)
                    }
                    Ok(_) | Err(_) => FolderWatchReport::ChangesTheOsDidNotName,
                };
                let _ = folder_reports_to_the_gatherer.send(folder_report);
            })?;
        folder_watcher.watch(resolved_project_directory, RecursiveMode::Recursive)?;
        let project_directory = resolved_project_directory.to_path_buf();
        std::thread::Builder::new()
            .name("project-files-watch".to_owned())
            .spawn(move || {
                gather_and_tell_until_the_watch_ends(
                    &project_directory,
                    &folder_reports,
                    &tell_of_the_files_changed,
                )
            })
            .map_err(notify::Error::io)?;
        Ok(Self {
            _folder_watcher: folder_watcher,
        })
    }
}

fn gather_and_tell_until_the_watch_ends(
    project_directory: &Path,
    folder_reports: &mpsc::Receiver<FolderWatchReport>,
    tell_of_the_files_changed: &impl Fn(ProjectFilesToldChanged),
) {
    while let Ok(first_folder_report) = folder_reports.recv() {
        let mut gathered = ProjectFilesGathered::default();
        gathered.gather(project_directory, first_folder_report);
        let told_at = Instant::now() + PROJECT_FILE_CHANGES_GATHERED_FOR;
        loop {
            let left_to_gather = told_at.saturating_duration_since(Instant::now());
            if left_to_gather.is_zero() {
                break;
            }
            match folder_reports.recv_timeout(left_to_gather) {
                Ok(folder_report) => gathered.gather(project_directory, folder_report),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
        if let Some(told_changed) = gathered.into_told() {
            tell_of_the_files_changed(told_changed);
        }
    }
}

/// The changes of one burst, gathered until it is told.
#[derive(Default)]
struct ProjectFilesGathered {
    files_changed: BTreeSet<String>,
    some_change_the_os_did_not_name: bool,
}

impl ProjectFilesGathered {
    fn gather(&mut self, project_directory: &Path, folder_report: FolderWatchReport) {
        match folder_report {
            FolderWatchReport::ChangesTheOsDidNotName => {
                self.some_change_the_os_did_not_name = true
            }
            FolderWatchReport::Event(folder_event) => {
                if matches!(folder_event.kind, EventKind::Access(_)) {
                    return;
                }
                self.files_changed.extend(
                    folder_event
                        .paths
                        .iter()
                        .filter_map(|path| the_project_file_a_path_names(project_directory, path)),
                );
            }
        }
    }

    fn into_told(self) -> Option<ProjectFilesToldChanged> {
        if self.some_change_the_os_did_not_name {
            return Some(ProjectFilesToldChanged::SomeTheOsDidNotName);
        }
        (!self.files_changed.is_empty())
            .then(|| ProjectFilesToldChanged::These(self.files_changed.into_iter().collect()))
    }
}

/// `path` relative to `project_directory`, `/` between segments, or none when it lies outside the
/// project or inside a folder the project's history records nothing of.
fn the_project_file_a_path_names(project_directory: &Path, path: &Path) -> Option<String> {
    let relative_path = path.strip_prefix(project_directory).ok()?;
    let mut segments = Vec::new();
    let mut folder = PathBuf::from(project_directory);
    for component in relative_path.components() {
        let Component::Normal(segment) = component else {
            return None;
        };
        let segment = segment.to_str()?;
        if segment.starts_with('.') || DIRECTORIES_A_PROJECT_SCAN_SKIPS.contains(&segment) {
            return None;
        }
        if folder.join("pyvenv.cfg").exists() {
            return None;
        }
        folder.push(segment);
        segments.push(segment);
    }
    (!segments.is_empty()).then(|| segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_named_relative_to_the_project_and_what_history_skips_is_left_out() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join("env")).unwrap();
        std::fs::write(project.path().join("env/pyvenv.cfg"), b"").unwrap();
        let named = |relative: &str| {
            the_project_file_a_path_names(project.path(), &project.path().join(relative))
        };

        assert_eq!(named("nodes/blur.py").as_deref(), Some("nodes/blur.py"));
        assert_eq!(
            named("streams/main.graph.json").as_deref(),
            Some("streams/main.graph.json")
        );
        for left_out in [
            ".git/index",
            ".tatolab/store.redb",
            "views/panel/node_modules/left-pad/index.js",
            "nodes/__pycache__/blur.cpython-312.pyc",
            "env/lib/site.py",
        ] {
            assert_eq!(named(left_out), None, "{left_out}");
        }
        assert_eq!(
            the_project_file_a_path_names(project.path(), Path::new("/etc/hosts")),
            None
        );
    }

    /// A burst holding a change the OS did not name is told as such, so a client lists the
    /// project again; a burst of reads alone is not told.
    ///
    /// Mental-revert: dropping a rescan's word tells nothing, and the client keeps a stale listing.
    #[test]
    fn a_change_the_os_did_not_name_is_told_and_reads_alone_are_not() {
        let project = Path::new("/projects/arm");
        let mut gathered = ProjectFilesGathered::default();
        gathered.gather(
            project,
            FolderWatchReport::Event(
                notify::Event::new(EventKind::Modify(notify::event::ModifyKind::Any))
                    .add_path(project.join("nodes/blur.py")),
            ),
        );
        gathered.gather(project, FolderWatchReport::ChangesTheOsDidNotName);
        assert_eq!(
            gathered.into_told(),
            Some(ProjectFilesToldChanged::SomeTheOsDidNotName)
        );

        let mut reads_alone = ProjectFilesGathered::default();
        reads_alone.gather(
            project,
            FolderWatchReport::Event(
                notify::Event::new(EventKind::Access(notify::event::AccessKind::Any))
                    .add_path(project.join("nodes/blur.py")),
            ),
        );
        assert_eq!(reads_alone.into_told(), None);
    }

    /// A file written in the project's folder, outside any runtime call, is told once its burst
    /// is gathered; a write under `.git` is not.
    ///
    /// Mental-revert: without the watch nothing is told and the wait times out; without the
    /// filter the `.git` write is told too.
    #[test]
    fn a_file_written_in_the_folder_is_told_once_gathered_and_a_hidden_one_is_not() {
        let project = tempfile::tempdir().unwrap();
        let resolved_project_directory = std::fs::canonicalize(project.path()).unwrap();
        std::fs::create_dir_all(resolved_project_directory.join("nodes")).unwrap();
        std::fs::create_dir_all(resolved_project_directory.join(".git")).unwrap();
        let (files_told_changed_to_the_test, files_told_changed) =
            mpsc::channel::<ProjectFilesToldChanged>();
        let _watch = ProjectFilesChangedOutsideTheRuntimeWatch::of_the_project_in(
            &resolved_project_directory,
            move |files_changed| {
                let _ = files_told_changed_to_the_test.send(files_changed);
            },
        )
        .unwrap();

        std::fs::write(resolved_project_directory.join(".git/index"), b"index").unwrap();
        std::fs::write(resolved_project_directory.join("nodes/blur.py"), b"x = 1").unwrap();
        std::fs::write(resolved_project_directory.join("nodes/blur.py"), b"x = 2").unwrap();

        let files_named = |told_changed| match told_changed {
            ProjectFilesToldChanged::These(files_changed) => files_changed,
            ProjectFilesToldChanged::SomeTheOsDidNotName => panic!("the OS named no file"),
        };
        let mut every_file_told_changed = files_named(
            files_told_changed
                .recv_timeout(Duration::from_secs(5))
                .expect("the write is told"),
        );
        while let Ok(more_told_changed) =
            files_told_changed.recv_timeout(PROJECT_FILE_CHANGES_GATHERED_FOR * 3)
        {
            every_file_told_changed.extend(files_named(more_told_changed));
        }
        assert!(
            every_file_told_changed.contains(&"nodes/blur.py".to_owned()),
            "{every_file_told_changed:?}"
        );
        assert!(
            every_file_told_changed
                .iter()
                .all(|file| file.starts_with("nodes")),
            "{every_file_told_changed:?}"
        );
    }
}
