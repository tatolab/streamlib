// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How often `dev` rescans the project for an edit.
pub(crate) const PROJECT_SOURCE_SCAN_INTERVAL: Duration = Duration::from_millis(250);

/// Each watched file's modification time and size, keyed by path.
type ProjectSourceSnapshot = BTreeMap<PathBuf, (Option<SystemTime>, u64)>;

fn is_watched_project_source_file(file_name: &str) -> bool {
    file_name.ends_with(".py") || file_name == "pyproject.toml"
}

fn is_skipped_project_directory(directory_path: &Path, directory_name: &str) -> bool {
    directory_name.starts_with('.')
        || directory_name == "venv"
        || directory_name == "__pycache__"
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

/// Scan `project_anchor_directory` now, then call `on_project_sources_changed` from a polling
/// thread each time an edit settles: two consecutive scans agree on a state the last one
/// reported differs from. The thread ends when the callback returns `false`.
pub(crate) fn watch_project_sources(
    project_anchor_directory: PathBuf,
    on_project_sources_changed: impl Fn() -> bool + Send + 'static,
) -> std::io::Result<()> {
    let mut last_reported_snapshot = scan_project_sources(&project_anchor_directory);
    std::thread::Builder::new()
        .name("tatolab-dev-project-source-watcher".to_owned())
        .spawn(move || {
            let mut previous_scan_snapshot = last_reported_snapshot.clone();
            loop {
                std::thread::sleep(PROJECT_SOURCE_SCAN_INTERVAL);
                let current_scan_snapshot = scan_project_sources(&project_anchor_directory);
                if current_scan_snapshot != last_reported_snapshot
                    && current_scan_snapshot == previous_scan_snapshot
                {
                    last_reported_snapshot = current_scan_snapshot.clone();
                    if !on_project_sources_changed() {
                        return;
                    }
                }
                previous_scan_snapshot = current_scan_snapshot;
            }
        })?;
    Ok(())
}
