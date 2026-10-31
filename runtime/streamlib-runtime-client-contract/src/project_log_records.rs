// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reading a project's log from disk: its rotated segments by ascending
//! sequence, then its active one, a page at a time from an opaque cursor that
//! survives the runtime's restarts and the log's rotations.

use std::io;
use std::path::{Path, PathBuf};

use crate::runtime_log_file_paths::{
    project_log_path, rotated_runtime_log_segment_path,
    rotated_runtime_log_segment_sequences_on_disk,
};

/// One page of a project's log records.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectLogRecordsPage {
    /// The records, each as its JSONL line holds it, oldest first.
    pub records: Vec<serde_json::Value>,
    /// The cursor the next page starts after.
    pub next_after: String,
    /// Whether retention deleted records between the cursor and this page.
    pub records_no_longer_held: bool,
}

/// Why a project's log was not read.
#[derive(Debug, thiserror::Error)]
pub enum ProjectLogRecordsNotRead {
    /// The cursor is not one a page answered.
    #[error(
        "`{cursor}` is not a log cursor: a cursor is the `next_after` a page answered, \
         `<segment>:<line>`"
    )]
    CursorMalformed {
        /// The cursor as given.
        cursor: String,
    },
    /// A segment of the log could not be listed or read.
    #[error("the project log {path} could not be read: {source}")]
    SegmentUnreadable {
        /// The segment, or the log's directory.
        path: PathBuf,
        /// What the read reported.
        source: io::Error,
    },
}

/// Where a page starts or ends: the segment's sequence, the active one's
/// being one past the highest rotated, and how many of its lines come before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ProjectLogCursor {
    segment_sequence: u64,
    lines_before: usize,
}

impl ProjectLogCursor {
    fn parse(cursor: &str) -> Option<Self> {
        let (segment_sequence, lines_before) = cursor.split_once(':')?;
        Some(Self {
            segment_sequence: segment_sequence.parse().ok()?,
            lines_before: lines_before.parse().ok()?,
        })
    }

    fn spelled(self) -> String {
        format!("{}:{}", self.segment_sequence, self.lines_before)
    }
}

/// At most `count` records of the log of the project in `project_directory`
/// after the cursor `after` — from its oldest held record when `None` — each
/// written by the stream `stream` and for the stream action `action` when
/// they are given. A cursor whose segment retention deleted resumes at the
/// oldest held, saying so; a line still being written, or one that is not a
/// JSON record, is not answered.
pub fn project_log_records_after(
    project_directory: &Path,
    stream: Option<&str>,
    action: Option<&str>,
    after: Option<&str>,
    count: usize,
) -> Result<ProjectLogRecordsPage, ProjectLogRecordsNotRead> {
    let active_segment_path = project_log_path(project_directory);
    let rotated_sequences = match rotated_runtime_log_segment_sequences_on_disk(
        &active_segment_path,
    ) {
        Ok(rotated_sequences) => rotated_sequences,
        Err(not_listed) if not_listed.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(not_listed) => {
            return Err(ProjectLogRecordsNotRead::SegmentUnreadable {
                path: active_segment_path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default(),
                source: not_listed,
            });
        }
    };
    let active_segment_sequence = rotated_sequences.last().map_or(1, |highest| highest + 1);
    let mut segments: Vec<(u64, PathBuf)> = rotated_sequences
        .iter()
        .map(|&sequence| {
            (
                sequence,
                rotated_runtime_log_segment_path(&active_segment_path, sequence),
            )
        })
        .collect();
    segments.push((active_segment_sequence, active_segment_path));
    let oldest_held = ProjectLogCursor {
        segment_sequence: segments[0].0,
        lines_before: 0,
    };

    let mut start = match after {
        Some(cursor) => ProjectLogCursor::parse(cursor).ok_or_else(|| {
            ProjectLogRecordsNotRead::CursorMalformed {
                cursor: cursor.to_string(),
            }
        })?,
        None => oldest_held,
    };
    let records_no_longer_held = start.segment_sequence < oldest_held.segment_sequence;
    if records_no_longer_held {
        start = oldest_held;
    }
    if start.segment_sequence > active_segment_sequence {
        start = ProjectLogCursor {
            segment_sequence: active_segment_sequence,
            lines_before: 0,
        };
    }

    let mut records = Vec::new();
    let mut next_after = start;
    for (segment_sequence, segment_path) in segments
        .into_iter()
        .filter(|(sequence, _)| *sequence >= start.segment_sequence)
    {
        let lines_before = if segment_sequence == start.segment_sequence {
            start.lines_before
        } else {
            0
        };
        let segment = match std::fs::read(&segment_path) {
            Ok(segment) => segment,
            Err(not_read) if not_read.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(not_read) => {
                return Err(ProjectLogRecordsNotRead::SegmentUnreadable {
                    path: segment_path,
                    source: not_read,
                });
            }
        };
        let complete_lines: Vec<&[u8]> = segment
            .split_inclusive(|&byte| byte == b'\n')
            .filter(|line| line.ends_with(b"\n"))
            .collect();
        next_after = ProjectLogCursor {
            segment_sequence,
            lines_before,
        };
        for (line_index, line) in complete_lines.iter().enumerate().skip(lines_before) {
            next_after.lines_before = line_index + 1;
            let Ok(record) = serde_json::from_slice::<serde_json::Value>(line) else {
                continue;
            };
            if !the_record_names(&record, "stream", stream)
                || !the_record_names(&record, "action", action)
            {
                continue;
            }
            records.push(record);
            if records.len() >= count {
                return Ok(ProjectLogRecordsPage {
                    records,
                    next_after: next_after.spelled(),
                    records_no_longer_held,
                });
            }
        }
        next_after.lines_before = lines_before.max(complete_lines.len());
    }
    Ok(ProjectLogRecordsPage {
        records,
        next_after: next_after.spelled(),
        records_no_longer_held,
    })
}

/// Whether `record`'s `field` is `wanted`, or no value is wanted.
fn the_record_names(record: &serde_json::Value, field: &str, wanted: Option<&str>) -> bool {
    match wanted {
        Some(wanted) => record.get(field).and_then(serde_json::Value::as_str) == Some(wanted),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_log_file_paths::project_log_directory;

    fn a_record(stream: Option<&str>, action: &str, message: &str) -> String {
        let mut record = serde_json::json!({"action": action, "message": message});
        if let Some(stream) = stream {
            record["stream"] = serde_json::json!(stream);
        }
        format!("{record}\n")
    }

    fn messages_of(page: &ProjectLogRecordsPage) -> Vec<&str> {
        page.records
            .iter()
            .map(|record| record["message"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn records_are_read_oldest_segment_first_filtered_and_paged_from_the_cursor() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project_log_directory(project.path())).unwrap();
        let active = project_log_path(project.path());
        std::fs::write(
            rotated_runtime_log_segment_path(&active, 1),
            [
                a_record(None, "R-1", "queued"),
                a_record(Some("cam"), "R-1", "first"),
            ]
            .concat(),
        )
        .unwrap();
        std::fs::write(
            &active,
            [
                a_record(Some("mic"), "R-1", "other stream"),
                a_record(Some("cam"), "R-2", "second"),
                "{\"half a line".to_string(),
            ]
            .concat(),
        )
        .unwrap();

        let every = project_log_records_after(project.path(), None, None, None, 100).unwrap();
        assert_eq!(
            messages_of(&every),
            ["queued", "first", "other stream", "second"]
        );
        assert_eq!(every.next_after, "2:2", "the half-written line is not read");
        assert!(!every.records_no_longer_held);

        let first_page =
            project_log_records_after(project.path(), Some("cam"), None, None, 1).unwrap();
        assert_eq!(messages_of(&first_page), ["first"]);
        let second_page = project_log_records_after(
            project.path(),
            Some("cam"),
            None,
            Some(&first_page.next_after),
            10,
        )
        .unwrap();
        assert_eq!(messages_of(&second_page), ["second"]);

        let one_action =
            project_log_records_after(project.path(), None, Some("R-2"), None, 10).unwrap();
        assert_eq!(messages_of(&one_action), ["second"]);

        let nothing_new =
            project_log_records_after(project.path(), None, None, Some(&every.next_after), 10)
                .unwrap();
        assert!(nothing_new.records.is_empty());
        assert_eq!(nothing_new.next_after, every.next_after);
    }

    #[test]
    fn a_cursor_retention_overtook_resumes_at_the_oldest_held_saying_so() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project_log_directory(project.path())).unwrap();
        let active = project_log_path(project.path());
        std::fs::write(
            rotated_runtime_log_segment_path(&active, 5),
            a_record(None, "R-1", "oldest held"),
        )
        .unwrap();
        std::fs::write(&active, "").unwrap();

        let page =
            project_log_records_after(project.path(), None, None, Some("2:7"), 10).unwrap();

        assert!(page.records_no_longer_held);
        assert_eq!(messages_of(&page), ["oldest held"]);
        assert_eq!(page.next_after, "6:0");
    }

    #[test]
    fn a_project_with_no_log_answers_no_record_and_a_malformed_cursor_is_refused() {
        let project = tempfile::tempdir().unwrap();

        let page = project_log_records_after(project.path(), None, None, None, 10).unwrap();
        assert!(page.records.is_empty());
        assert_eq!(page.next_after, "1:0");

        let refusal =
            project_log_records_after(project.path(), None, None, Some("seven"), 10).unwrap_err();
        assert!(refusal.to_string().contains("`seven`"), "{refusal}");
    }
}
