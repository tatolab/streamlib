// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// How often `dev` rescans the project for an edit.
pub(crate) const PROJECT_SOURCE_SCAN_INTERVAL: Duration = Duration::from_millis(250);

/// Each watched file's modification time and size, keyed by path.
type ProjectSourceSnapshot = BTreeMap<PathBuf, (Option<SystemTime>, u64)>;

fn is_watched_project_source_file(file_name: &str) -> bool {
    file_name.ends_with(".py")
        || file_name.ends_with(".graph.json")
        || file_name == "pyproject.toml"
        || file_name == "tatolab-project.json"
}

fn is_skipped_project_directory(directory_path: &Path, directory_name: &str) -> bool {
    directory_name.starts_with('.')
        || directory_name == "venv"
        || directory_name == "__pycache__"
        || directory_name == "node_modules"
        || directory_path.join("pyvenv.cfg").exists()
}

fn scan_project_sources_into(directory_path: &Path, snapshot: &mut ProjectSourceSnapshot) {
    let Ok(directory_entries) = fs::read_dir(directory_path) else {
        return;
    };
    for directory_entry in directory_entries.flatten() {
        let Ok(entry_file_type) = directory_entry.file_type() else {
            continue;
        };
        let entry_name = directory_entry.file_name().to_string_lossy().into_owned();
        let entry_path = directory_entry.path();
        if entry_file_type.is_dir() {
            if !is_skipped_project_directory(&entry_path, &entry_name) {
                scan_project_sources_into(&entry_path, snapshot);
            }
        } else if is_watched_project_source_file(&entry_name)
            && let Ok(entry_metadata) = fs::metadata(&entry_path)
        {
            snapshot.insert(
                entry_path,
                (entry_metadata.modified().ok(), entry_metadata.len()),
            );
        }
    }
}

fn scan_project_sources(project_anchor_directory: &Path) -> ProjectSourceSnapshot {
    let mut snapshot = ProjectSourceSnapshot::new();
    scan_project_sources_into(project_anchor_directory, &mut snapshot);
    snapshot
}

/// Holds back a [`watch_project_sources`] watch's reports while `dev`'s runtime writes the
/// project's graph documents, then takes each graph document as it stands at the release as
/// already seen; every other file's change, and a later save of a graph document, is reported.
#[derive(Clone)]
pub(crate) struct ProjectSourceWatchHold {
    project_anchor_directory: PathBuf,
    reports_held_back: Arc<AtomicBool>,
    graph_documents_as_released: Arc<Mutex<Option<ProjectSourceSnapshot>>>,
}

impl ProjectSourceWatchHold {
    /// Hold back every report until [`Self::release_taking_the_graph_documents_as_seen`].
    pub(crate) fn hold_back_the_reports(&self) {
        self.reports_held_back.store(true, Ordering::SeqCst);
    }

    /// Report again, each graph document as it stands now taken as already reported.
    pub(crate) fn release_taking_the_graph_documents_as_seen(&self) {
        let mut graph_documents_now = scan_project_sources(&self.project_anchor_directory);
        graph_documents_now.retain(|path, _| is_a_graph_document(path));
        *self
            .graph_documents_as_released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(graph_documents_now);
        self.reports_held_back.store(false, Ordering::SeqCst);
    }
}

fn is_a_graph_document(path: &Path) -> bool {
    path.file_name()
        .and_then(|file_name| file_name.to_str())
        .is_some_and(|file_name| file_name.ends_with(".graph.json"))
}

/// `snapshot` with each graph document as `graph_documents` holds them, every other file as it
/// was.
fn with_the_graph_documents_of(
    snapshot: &mut ProjectSourceSnapshot,
    graph_documents: &ProjectSourceSnapshot,
) {
    snapshot.retain(|path, _| !is_a_graph_document(path));
    snapshot.extend(
        graph_documents
            .iter()
            .map(|(path, state)| (path.clone(), *state)),
    );
}

/// Every watched file whose state differs between `before` and `after`, removed ones included.
fn files_that_changed_between(
    before: &ProjectSourceSnapshot,
    after: &ProjectSourceSnapshot,
) -> Vec<PathBuf> {
    after
        .iter()
        .filter(|(path, state)| before.get(*path) != Some(state))
        .map(|(path, _)| path.clone())
        .chain(
            before
                .keys()
                .filter(|path| !after.contains_key(*path))
                .cloned(),
        )
        .collect()
}

/// Scan `project_anchor_directory` now, then call `on_project_sources_changed` from a polling
/// thread with the files that changed each time an edit settles: two consecutive scans agree on
/// a state the last one reported differs from. The thread ends when the callback returns
/// `false`; the hold it answers keeps it quiet while the caller writes the project.
pub(crate) fn watch_project_sources(
    project_anchor_directory: PathBuf,
    on_project_sources_changed: impl Fn(Vec<PathBuf>) -> bool + Send + 'static,
) -> std::io::Result<ProjectSourceWatchHold> {
    let watch_hold = ProjectSourceWatchHold {
        project_anchor_directory: project_anchor_directory.clone(),
        reports_held_back: Arc::new(AtomicBool::new(false)),
        graph_documents_as_released: Arc::new(Mutex::new(None)),
    };
    let held_by_the_thread = watch_hold.clone();
    let mut last_reported_snapshot = scan_project_sources(&project_anchor_directory);
    std::thread::Builder::new()
        .name("tatolab-dev-project-source-watcher".to_owned())
        .spawn(move || {
            let mut previous_scan_snapshot = last_reported_snapshot.clone();
            loop {
                std::thread::sleep(PROJECT_SOURCE_SCAN_INTERVAL);
                let current_scan_snapshot = scan_project_sources(&project_anchor_directory);
                if held_by_the_thread.reports_held_back.load(Ordering::SeqCst) {
                    previous_scan_snapshot = current_scan_snapshot;
                    continue;
                }
                if let Some(graph_documents_as_released) = held_by_the_thread
                    .graph_documents_as_released
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                {
                    with_the_graph_documents_of(
                        &mut last_reported_snapshot,
                        &graph_documents_as_released,
                    );
                    with_the_graph_documents_of(
                        &mut previous_scan_snapshot,
                        &graph_documents_as_released,
                    );
                }
                if current_scan_snapshot != last_reported_snapshot
                    && current_scan_snapshot == previous_scan_snapshot
                {
                    let changed_files =
                        files_that_changed_between(&last_reported_snapshot, &current_scan_snapshot);
                    last_reported_snapshot = current_scan_snapshot.clone();
                    if !on_project_sources_changed(changed_files) {
                        return;
                    }
                }
                previous_scan_snapshot = current_scan_snapshot;
            }
        })?;
    Ok(watch_hold)
}
