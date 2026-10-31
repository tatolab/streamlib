// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! JSONL log directory resolution and the segment file names a project's log and the
//! runtime's own log are written under and read back by.

use std::io;
use std::path::{Path, PathBuf};

/// `<project_directory>/.streamlib/logs/`, where a project's one log lives.
pub fn project_log_directory(project_directory: &Path) -> PathBuf {
    project_directory.join(".streamlib").join("logs")
}

/// The file name of a project log's active segment; a rotated one is
/// `project.<seq>.jsonl`.
pub const PROJECT_LOG_ACTIVE_SEGMENT_FILE_NAME: &str = "project.jsonl";

/// Path of the active JSONL segment of the project in `project_directory`:
/// `<project_directory>/.streamlib/logs/project.jsonl`, appended to across the runtime's runs.
pub fn project_log_path(project_directory: &Path) -> PathBuf {
    project_log_directory(project_directory).join(PROJECT_LOG_ACTIVE_SEGMENT_FILE_NAME)
}

/// The active segment's file name for one log instance:
/// `<instance>-<started_at_millis>.jsonl`.
pub fn active_runtime_log_segment_file_name(
    runtime_id: &str,
    started_at_millis: impl std::fmt::Display,
) -> String {
    format!("{runtime_id}-{started_at_millis}.jsonl")
}

/// Path a rotated segment of `active_segment_path` is renamed to:
/// `<runtime_id>-<started_at_millis>.<rotation_sequence>.jsonl`.
///
/// The sequence is dot-separated because a pinned `STREAMLIB_RUNTIME_ID` may
/// carry dashes, and `camera-2-1700000000000.jsonl` would read two ways.
pub fn rotated_runtime_log_segment_path(
    active_segment_path: &Path,
    rotation_sequence: u64,
) -> PathBuf {
    active_segment_path.with_extension(format!("{rotation_sequence}.jsonl"))
}

/// The rotation sequence `candidate_file_name` carries when it names a rotated
/// segment of `active_segment_path` — the inverse of [`rotated_runtime_log_segment_path`].
pub fn rotated_runtime_log_segment_sequence(
    active_segment_path: &Path,
    candidate_file_name: &str,
) -> Option<u64> {
    let active_stem = active_segment_path.file_stem()?.to_str()?;
    let sequence_digits = candidate_file_name
        .strip_prefix(active_stem)?
        .strip_prefix('.')?
        .strip_suffix(".jsonl")?;
    if !is_ascii_digits(sequence_digits) {
        return None;
    }
    sequence_digits.parse().ok()
}

/// Path a rotation creates the next active segment at before renaming it into place.
pub fn replacement_runtime_log_segment_path(active_segment_path: &Path) -> PathBuf {
    active_segment_path.with_extension("jsonl.rotating")
}

/// The sequence of every rotated segment of `active_segment_path` on disk, oldest first.
pub fn rotated_runtime_log_segment_sequences_on_disk(
    active_segment_path: &Path,
) -> io::Result<Vec<u64>> {
    let directory = match active_segment_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let mut rotated_sequences = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let file_name = entry?.file_name();
        if let Some(rotated_sequence) = file_name
            .to_str()
            .and_then(|name| rotated_runtime_log_segment_sequence(active_segment_path, name))
        {
            rotated_sequences.push(rotated_sequence);
        }
    }
    rotated_sequences.sort_unstable();
    Ok(rotated_sequences)
}

fn is_ascii_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The active segment of `runtime_id` started at `started_at_millis`, in `/logs`.
    fn a_runtime_log_path(runtime_id: &str, started_at_millis: u128) -> PathBuf {
        Path::new("/logs").join(active_runtime_log_segment_file_name(
            runtime_id,
            started_at_millis,
        ))
    }

    #[test]
    fn an_active_segment_has_a_stable_name() {
        assert_eq!(
            active_runtime_log_segment_file_name("Rabc123", 1_700_000_000_000_u128),
            "Rabc123-1700000000000.jsonl"
        );
    }

    #[test]
    fn a_projects_log_lives_under_it_and_rotates_beside_its_active_segment() {
        let active_segment_path = project_log_path(Path::new("/projects/camera-app"));

        assert_eq!(
            active_segment_path,
            Path::new("/projects/camera-app/.streamlib/logs/project.jsonl")
        );
        assert_eq!(
            rotated_runtime_log_segment_path(&active_segment_path, 4),
            Path::new("/projects/camera-app/.streamlib/logs/project.4.jsonl")
        );
        assert_eq!(
            rotated_runtime_log_segment_sequence(&active_segment_path, "project.4.jsonl"),
            Some(4)
        );
    }

    /// The name a reader finds a runtime's rotated segments by, parsing it back
    /// with [`rotated_runtime_log_segment_sequence`].
    #[test]
    fn a_rotated_segment_is_named_with_a_dot_separated_sequence() {
        let active_segment_path = a_runtime_log_path("Rabc123", 1_700_000_000_000);
        let rotated = rotated_runtime_log_segment_path(&active_segment_path, 3);

        assert_eq!(rotated.parent(), active_segment_path.parent());
        assert_eq!(
            rotated.file_name().unwrap(),
            "Rabc123-1700000000000.3.jsonl"
        );
    }

    #[test]
    fn filename_shape_round_trips() {
        for (active_segment_path, rotation_sequence) in [
            (a_runtime_log_path("Rabc123", 1_700_000_000_000), 1),
            (a_runtime_log_path("Rabc123", 1_700_000_000_000), 3),
            (PathBuf::from("/logs/my.node-2-1700000000000.jsonl"), 12),
            (PathBuf::from("/logs/camera-2-1000.jsonl"), u64::MAX),
        ] {
            let rotated = rotated_runtime_log_segment_path(&active_segment_path, rotation_sequence);
            let rotated_file_name = rotated.file_name().unwrap().to_str().unwrap();

            assert_eq!(
                rotated_runtime_log_segment_sequence(&active_segment_path, rotated_file_name),
                Some(rotation_sequence),
                "{rotated_file_name} does not parse back to {rotation_sequence}"
            );
        }
    }

    #[test]
    fn a_file_that_is_not_one_of_this_segments_rotations_parses_to_no_sequence() {
        let active_segment_path = Path::new("/logs/Rabc-1000.jsonl");

        for foreign_file_name in [
            "Rabc-1000.jsonl",
            "Rabc-10000.7.jsonl",
            "Rabc-100.7.jsonl",
            "Rabc-1000.+5.jsonl",
            "Rabc-1000..jsonl",
            "Rabc-1000.5a.jsonl",
            "Rabc-1000.jsonl.rotating",
            "Rabc-1000.99999999999999999999999.jsonl",
        ] {
            assert_eq!(
                rotated_runtime_log_segment_sequence(active_segment_path, foreign_file_name),
                None,
                "{foreign_file_name} was read as a rotation of Rabc-1000.jsonl"
            );
        }
        assert_eq!(
            replacement_runtime_log_segment_path(active_segment_path),
            Path::new("/logs/Rabc-1000.jsonl.rotating")
        );
    }

    #[test]
    fn a_runtime_id_carrying_dots_and_dashes_keeps_its_whole_name_when_rotated() {
        let active_segment_path = Path::new("/logs/my.node-2-1700000000000.jsonl");

        assert_eq!(
            rotated_runtime_log_segment_path(active_segment_path, 12),
            Path::new("/logs/my.node-2-1700000000000.12.jsonl")
        );
    }
}
