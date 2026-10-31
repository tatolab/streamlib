// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reading a project's log from disk: its rotated segments by ascending
//! sequence, then its active one, a page at a time from an opaque cursor that
//! survives the runtime's restarts and the log's rotations.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
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
         `<segment>:<byte offset>`"
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
/// being one past the highest rotated, and the byte offset of the line after
/// the last one read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ProjectLogCursor {
    segment_sequence: u64,
    byte_offset: u64,
}

impl ProjectLogCursor {
    fn parse(cursor: &str) -> Option<Self> {
        let (segment_sequence, byte_offset) = cursor.split_once(':')?;
        Some(Self {
            segment_sequence: segment_sequence.parse().ok()?,
            byte_offset: byte_offset.parse().ok()?,
        })
    }

    fn spelled(self) -> String {
        format!("{}:{}", self.segment_sequence, self.byte_offset)
    }
}

/// How many times a page is read again when the log rotated under the read.
const PROJECT_LOG_READS_BEFORE_A_ROTATION_IS_ANSWERED_AS_IT_WAS: usize = 3;

/// At most `count` records of the log of the project in `project_directory`
/// after the cursor `after` — from its oldest held record when `None` — each
/// written by the stream `stream` and for the stream action `action` when
/// they are given. A cursor whose segment retention deleted resumes at the
/// oldest held, saying so; a line still being written, or one that is not a
/// JSON record, is not answered. A page the log rotated under is read again.
pub fn project_log_records_after(
    project_directory: &Path,
    stream: Option<&str>,
    action: Option<&str>,
    after: Option<&str>,
    count: usize,
) -> Result<ProjectLogRecordsPage, ProjectLogRecordsNotRead> {
    let active_segment_path = project_log_path(project_directory);
    let mut reads_left = PROJECT_LOG_READS_BEFORE_A_ROTATION_IS_ANSWERED_AS_IT_WAS;
    loop {
        reads_left -= 1;
        let rotated_sequences = rotated_project_log_sequences(&active_segment_path)?;
        let page = project_log_records_after_one_listing(
            &active_segment_path,
            &rotated_sequences,
            stream,
            action,
            after,
            count,
        )?;
        let the_log_rotated_under_the_read = page.a_segment_listed_was_gone
            || rotated_project_log_sequences(&active_segment_path)? != rotated_sequences;
        if !the_log_rotated_under_the_read || reads_left == 0 {
            return Ok(page.page);
        }
    }
}

/// The sequences of the rotated segments beside `active_segment_path`,
/// ascending; none when the log's directory does not exist.
fn rotated_project_log_sequences(
    active_segment_path: &Path,
) -> Result<Vec<u64>, ProjectLogRecordsNotRead> {
    match rotated_runtime_log_segment_sequences_on_disk(active_segment_path) {
        Ok(rotated_sequences) => Ok(rotated_sequences),
        Err(not_listed) if not_listed.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(not_listed) => Err(ProjectLogRecordsNotRead::SegmentUnreadable {
            path: active_segment_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            source: not_listed,
        }),
    }
}

/// A page read against one listing of the rotated segments, and whether a
/// segment that listing named was gone when opened.
struct ProjectLogRecordsPageOfOneListing {
    page: ProjectLogRecordsPage,
    a_segment_listed_was_gone: bool,
}

/// [`project_log_records_after`] against the listing `rotated_sequences`; a
/// listed segment gone when opened ends the page before it.
fn project_log_records_after_one_listing(
    active_segment_path: &Path,
    rotated_sequences: &[u64],
    stream: Option<&str>,
    action: Option<&str>,
    after: Option<&str>,
    count: usize,
) -> Result<ProjectLogRecordsPageOfOneListing, ProjectLogRecordsNotRead> {
    let active_segment_path = active_segment_path.to_path_buf();
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
        byte_offset: 0,
    };

    let mut start = match after {
        Some(cursor) => ProjectLogCursor::parse(cursor).ok_or_else(|| {
            ProjectLogRecordsNotRead::CursorMalformed {
                cursor: cursor.to_string(),
            }
        })?,
        None => oldest_held,
    };
    let mut records_no_longer_held = false;
    if start.segment_sequence < oldest_held.segment_sequence {
        records_no_longer_held = true;
        start = oldest_held;
    }
    if start.segment_sequence > active_segment_sequence {
        records_no_longer_held = true;
        start = ProjectLogCursor {
            segment_sequence: active_segment_sequence,
            byte_offset: 0,
        };
    }

    let mut records = Vec::new();
    let mut next_after = start;
    for (segment_sequence, segment_path) in segments
        .into_iter()
        .filter(|(sequence, _)| *sequence >= start.segment_sequence)
    {
        let mut byte_offset = if segment_sequence == start.segment_sequence {
            start.byte_offset
        } else {
            0
        };
        let segment = match File::open(&segment_path) {
            Ok(segment) => segment,
            Err(not_read) if not_read.kind() == io::ErrorKind::NotFound => {
                return Ok(ProjectLogRecordsPageOfOneListing {
                    page: ProjectLogRecordsPage {
                        records,
                        next_after: next_after.spelled(),
                        records_no_longer_held,
                    },
                    a_segment_listed_was_gone: true,
                });
            }
            Err(not_read) => {
                return Err(ProjectLogRecordsNotRead::SegmentUnreadable {
                    path: segment_path,
                    source: not_read,
                });
            }
        };
        let unreadable = |source: io::Error| ProjectLogRecordsNotRead::SegmentUnreadable {
            path: segment_path.clone(),
            source,
        };
        if !the_offset_starts_a_line(&segment, byte_offset).map_err(unreadable)? {
            records_no_longer_held = true;
            byte_offset = 0;
        }
        let mut segment_lines = BufReader::new(segment);
        segment_lines
            .seek(SeekFrom::Start(byte_offset))
            .map_err(unreadable)?;
        next_after = ProjectLogCursor {
            segment_sequence,
            byte_offset,
        };
        let mut line = Vec::new();
        loop {
            line.clear();
            let line_length = segment_lines
                .read_until(b'\n', &mut line)
                .map_err(unreadable)?;
            if line_length == 0 || !line.ends_with(b"\n") {
                break;
            }
            next_after.byte_offset += line_length as u64;
            let Ok(record) = serde_json::from_slice::<serde_json::Value>(&line) else {
                continue;
            };
            if !the_record_names(&record, "stream", stream)
                || !the_record_names(&record, "action", action)
            {
                continue;
            }
            records.push(record);
            if records.len() >= count {
                return Ok(ProjectLogRecordsPageOfOneListing {
                    page: ProjectLogRecordsPage {
                        records,
                        next_after: next_after.spelled(),
                        records_no_longer_held,
                    },
                    a_segment_listed_was_gone: false,
                });
            }
        }
    }
    Ok(ProjectLogRecordsPageOfOneListing {
        page: ProjectLogRecordsPage {
            records,
            next_after: next_after.spelled(),
            records_no_longer_held,
        },
        a_segment_listed_was_gone: false,
    })
}

/// Whether `byte_offset` is where a line of `segment` starts: its start, or
/// just past a newline it holds. A rotation that kept no rotated segment
/// leaves an active segment a cursor of the old one does not fit.
fn the_offset_starts_a_line(segment: &File, byte_offset: u64) -> io::Result<bool> {
    if byte_offset == 0 {
        return Ok(true);
    }
    if segment.metadata()?.len() < byte_offset {
        return Ok(false);
    }
    let mut byte_before = [0_u8];
    let mut segment = segment;
    segment.seek(SeekFrom::Start(byte_offset - 1))?;
    segment.read_exact(&mut byte_before)?;
    Ok(byte_before[0] == b'\n')
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
        let bytes_of_the_two_complete_lines = (a_record(Some("mic"), "R-1", "other stream").len()
            + a_record(Some("cam"), "R-2", "second").len())
            as u64;
        assert_eq!(
            every.next_after,
            format!("2:{bytes_of_the_two_complete_lines}"),
            "the half-written line is not read"
        );
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

        let page = project_log_records_after(project.path(), None, None, Some("2:7"), 10).unwrap();

        assert!(page.records_no_longer_held);
        assert_eq!(messages_of(&page), ["oldest held"]);
        assert_eq!(page.next_after, "6:0");
    }

    #[test]
    fn a_cursor_the_active_segment_no_longer_fits_resumes_at_its_start_saying_so() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project_log_directory(project.path())).unwrap();
        let active = project_log_path(project.path());
        std::fs::write(&active, a_record(None, "R-1", "after the rotation")).unwrap();

        let past_its_end =
            project_log_records_after(project.path(), None, None, Some("1:4096"), 10).unwrap();
        assert!(past_its_end.records_no_longer_held);
        assert_eq!(messages_of(&past_its_end), ["after the rotation"]);

        let inside_a_line =
            project_log_records_after(project.path(), None, None, Some("1:3"), 10).unwrap();
        assert!(inside_a_line.records_no_longer_held);
        assert_eq!(messages_of(&inside_a_line), ["after the rotation"]);

        let past_the_active_segment =
            project_log_records_after(project.path(), None, None, Some("9:0"), 10).unwrap();
        assert!(past_the_active_segment.records_no_longer_held);
        assert_eq!(
            messages_of(&past_the_active_segment),
            ["after the rotation"]
        );
    }

    #[test]
    fn a_listed_segment_gone_when_opened_ends_the_page_before_it_without_passing_it() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project_log_directory(project.path())).unwrap();
        let active = project_log_path(project.path());
        let first_line = a_record(None, "R-1", "in the first rotated");
        std::fs::write(rotated_runtime_log_segment_path(&active, 1), &first_line).unwrap();
        std::fs::write(&active, a_record(None, "R-1", "in the active")).unwrap();

        let read_against_a_stale_listing =
            project_log_records_after_one_listing(&active, &[1, 2], None, None, None, 10).unwrap();

        assert!(read_against_a_stale_listing.a_segment_listed_was_gone);
        assert_eq!(
            messages_of(&read_against_a_stale_listing.page),
            ["in the first rotated"]
        );
        assert_eq!(
            read_against_a_stale_listing.page.next_after,
            format!("1:{}", first_line.len())
        );
        let read_again = project_log_records_after(project.path(), None, None, None, 10).unwrap();
        assert_eq!(
            messages_of(&read_again),
            ["in the first rotated", "in the active"]
        );
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
