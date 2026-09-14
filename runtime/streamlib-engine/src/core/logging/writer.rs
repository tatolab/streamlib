// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Batched append-only JSONL writer. Size + time flush; final `fdatasync`
//! on clean shutdown only; size-triggered segment rotation with retention.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};

use crate::core::logging::paths::{
    replacement_runtime_log_segment_path, rotated_runtime_log_segment_path,
    rotated_runtime_log_segment_sequence,
};

/// When the active JSONL segment rolls over, and how many segments survive it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct JsonlSegmentRotationPolicy {
    /// Bytes after which the active segment rolls over; `None` never rotates.
    pub rotate_at_segment_bytes: Option<NonZeroU64>,
    /// Segments kept per runtime, the active one included; `None` keeps every one.
    pub retained_segment_count: Option<NonZeroUsize>,
}

#[cfg(test)]
impl JsonlSegmentRotationPolicy {
    /// A policy that never rotates, so the one segment grows for the runtime's life.
    pub const NEVER_ROTATE: Self = Self {
        rotate_at_segment_bytes: None,
        retained_segment_count: None,
    };
}

pub(crate) struct JsonlBatchedWriter {
    active_segment_path: PathBuf,
    active_segment_file: File,
    active_segment_bytes: u64,
    next_rotated_segment_sequence: u64,
    rotation_policy: JsonlSegmentRotationPolicy,
    buffer: Vec<u8>,
    batch_bytes: usize,
    fsync_on_every_batch: bool,
}

impl JsonlBatchedWriter {
    pub fn open(
        path: &Path,
        batch_bytes: usize,
        fsync_on_every_batch: bool,
        rotation_policy: JsonlSegmentRotationPolicy,
    ) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let active_segment_file = open_segment_for_append(path)?;
        let active_segment_bytes = active_segment_file.metadata()?.len();
        let next_rotated_segment_sequence = highest_rotated_segment_sequence_on_disk(path)? + 1;
        Ok(Self {
            active_segment_path: path.to_path_buf(),
            active_segment_file,
            active_segment_bytes,
            next_rotated_segment_sequence,
            rotation_policy,
            buffer: Vec::with_capacity(batch_bytes.saturating_add(1024)),
            batch_bytes,
            fsync_on_every_batch,
        })
    }

    /// Append one JSONL record to the buffer. `bytes` must be the
    /// serialized JSON without the trailing newline; this method adds the
    /// newline. Triggers a flush when the buffer exceeds `batch_bytes`.
    pub fn append_record(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.buffer.extend_from_slice(bytes);
        self.buffer.push(b'\n');
        if self.buffer.len() >= self.batch_bytes {
            self.flush_buffer()?;
        }
        Ok(())
    }

    /// Flush buffered records to the OS if any are pending. Time-triggered
    /// callers use this between flushes.
    pub fn flush_if_pending(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        self.flush_buffer()
    }

    /// Final flush + `fdatasync`. Only use on clean shutdown — hard-crash
    /// paths (panic hook, SIGKILL) should call [`flush_if_pending`] at
    /// most, and accept up to one buffer's worth of loss per the
    /// durability contract. Never rotates: the synced segment is the one
    /// the last records landed in.
    ///
    /// [`flush_if_pending`]: Self::flush_if_pending
    pub fn flush_and_fsync(&mut self) -> io::Result<()> {
        self.write_buffer_to_active_segment()?;
        self.active_segment_file.sync_data()
    }

    fn flush_buffer(&mut self) -> io::Result<()> {
        self.write_buffer_to_active_segment()?;
        if self.fsync_on_every_batch {
            self.active_segment_file.sync_data()?;
        }
        if self
            .rotation_policy
            .rotate_at_segment_bytes
            .is_some_and(|limit| self.active_segment_bytes >= limit.get())
        {
            self.rotate_active_segment()?;
        }
        Ok(())
    }

    fn write_buffer_to_active_segment(&mut self) -> io::Result<()> {
        // The buffer is newline-aligned by construction — every
        // `append_record` appends `bytes + '\n'` as a unit — and rotation
        // only happens between whole buffers, so no record straddles two
        // segments.
        self.active_segment_file.write_all(&self.buffer)?;
        self.active_segment_bytes += self.buffer.len() as u64;
        self.buffer.clear();
        Ok(())
    }

    fn rotate_active_segment(&mut self) -> io::Result<()> {
        let rotated_sequence = self.next_rotated_segment_sequence;
        let rotated_path =
            rotated_runtime_log_segment_path(&self.active_segment_path, rotated_sequence);
        let replacement_path = replacement_runtime_log_segment_path(&self.active_segment_path);
        // The replacement is created before either rename, so a failed open
        // (EMFILE, ENOSPC) leaves the active name where it was. `set_len` clears
        // a replacement a crash left behind, since std refuses append + truncate.
        let replacement_file = open_segment_for_append(&replacement_path)?;
        if let Err(clear_failure) = replacement_file.set_len(0) {
            let _ = std::fs::remove_file(&replacement_path);
            return Err(clear_failure);
        }
        if let Err(rename_failure) = std::fs::rename(&self.active_segment_path, &rotated_path) {
            let _ = std::fs::remove_file(&replacement_path);
            return Err(rename_failure);
        }
        if let Err(rename_failure) = std::fs::rename(&replacement_path, &self.active_segment_path) {
            let _ = std::fs::rename(&rotated_path, &self.active_segment_path);
            let _ = std::fs::remove_file(&replacement_path);
            return Err(rename_failure);
        }
        self.active_segment_file = replacement_file;
        self.active_segment_bytes = 0;
        self.next_rotated_segment_sequence += 1;
        self.delete_rotated_segment_past_retention(rotated_sequence)
    }

    fn delete_rotated_segment_past_retention(
        &self,
        newest_rotated_sequence: u64,
    ) -> io::Result<()> {
        let Some(retained_segment_count) = self.rotation_policy.retained_segment_count else {
            return Ok(());
        };
        // The active segment is one of the retained ones.
        let retained_rotated_segment_count = retained_segment_count.get() as u64 - 1;
        if newest_rotated_sequence <= retained_rotated_segment_count {
            return Ok(());
        }
        let expired_path = rotated_runtime_log_segment_path(
            &self.active_segment_path,
            newest_rotated_sequence - retained_rotated_segment_count,
        );
        match std::fs::remove_file(&expired_path) {
            Err(removal_failure) if removal_failure.kind() != io::ErrorKind::NotFound => {
                Err(removal_failure)
            }
            _ => Ok(()),
        }
    }
}

fn open_segment_for_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

/// The highest `<seq>` among `active_segment_path`'s rotated segments, or `0`.
fn highest_rotated_segment_sequence_on_disk(active_segment_path: &Path) -> io::Result<u64> {
    let directory = match active_segment_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let mut highest_rotated_sequence = 0;
    for entry in std::fs::read_dir(directory)? {
        let file_name = entry?.file_name();
        if let Some(rotated_sequence) = file_name
            .to_str()
            .and_then(|name| rotated_runtime_log_segment_sequence(active_segment_path, name))
        {
            highest_rotated_sequence = highest_rotated_sequence.max(rotated_sequence);
        }
    }
    Ok(highest_rotated_sequence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn read_all(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    fn rotating_every(bytes: u64, retained: Option<usize>) -> JsonlSegmentRotationPolicy {
        JsonlSegmentRotationPolicy {
            rotate_at_segment_bytes: Some(NonZeroU64::new(bytes).unwrap()),
            retained_segment_count: retained.map(|count| NonZeroUsize::new(count).unwrap()),
        }
    }

    fn numbered_record(sequence: u64) -> Vec<u8> {
        format!(
            r#"{{"sequence":{sequence},"padding":"{}"}}"#,
            "x".repeat(80)
        )
        .into_bytes()
    }

    /// Every file in `directory`, sorted by name.
    fn every_file_name(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Every `*.jsonl` file in `directory`, sorted by name.
    fn segment_file_names(directory: &Path) -> Vec<String> {
        every_file_name(directory)
            .into_iter()
            .filter(|name| name.ends_with(".jsonl"))
            .collect()
    }

    /// Every record's `sequence`, across every segment, asserting each line parses.
    fn sequences_across_segments(directory: &Path) -> Vec<u64> {
        let mut sequences = Vec::new();
        for name in segment_file_names(directory) {
            let contents = read_all(&directory.join(&name));
            assert!(
                contents.is_empty() || contents.ends_with('\n'),
                "segment {name} ends mid-record"
            );
            for line in contents.lines() {
                let record: serde_json::Value = serde_json::from_str(line)
                    .unwrap_or_else(|e| panic!("segment {name} holds a torn line {line:?}: {e}"));
                sequences.push(record["sequence"].as_u64().unwrap());
            }
        }
        sequences.sort_unstable();
        sequences
    }

    #[test]
    fn records_accumulate_until_size_flush() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("out.jsonl");
        let mut w =
            JsonlBatchedWriter::open(&path, 32, false, JsonlSegmentRotationPolicy::NEVER_ROTATE)
                .unwrap();

        // Two 20-byte records (+\n) → first flush after second append.
        w.append_record(b"{\"k\":\"aaaaaaaaaaaaa\"}").unwrap(); // 21 bytes + \n = 22
        assert_eq!(read_all(&path), "");
        w.append_record(b"{\"k\":\"bbbbbbbbbbbbb\"}").unwrap(); // 21 bytes + \n = 22, buffer now 44 >= 32 → flush
        let contents = read_all(&path);
        assert!(contents.contains("aaaaaaaaaaaaa"));
        assert!(contents.contains("bbbbbbbbbbbbb"));
        // Every record must end with a newline — no torn lines.
        assert!(contents.ends_with('\n'));
        for line in contents.lines() {
            assert!(line.starts_with('{'));
            assert!(line.ends_with('}'));
        }
    }

    #[test]
    fn flush_if_pending_emits_buffered() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("out.jsonl");
        let mut w = JsonlBatchedWriter::open(
            &path,
            1024 * 1024,
            false,
            JsonlSegmentRotationPolicy::NEVER_ROTATE,
        )
        .unwrap();
        w.append_record(b"{\"a\":1}").unwrap();
        assert_eq!(read_all(&path), "");
        w.flush_if_pending().unwrap();
        assert_eq!(read_all(&path), "{\"a\":1}\n");
    }

    #[test]
    fn flush_and_fsync_persists() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("out.jsonl");
        let mut w = JsonlBatchedWriter::open(
            &path,
            1024 * 1024,
            false,
            JsonlSegmentRotationPolicy::NEVER_ROTATE,
        )
        .unwrap();
        w.append_record(b"{\"a\":1}").unwrap();
        w.flush_and_fsync().unwrap();
        assert_eq!(read_all(&path), "{\"a\":1}\n");
    }

    #[test]
    fn append_opens_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nested").join("dirs").join("out.jsonl");
        let mut w = JsonlBatchedWriter::open(
            &path,
            1024 * 1024,
            false,
            JsonlSegmentRotationPolicy::NEVER_ROTATE,
        )
        .unwrap();
        w.append_record(b"{}").unwrap();
        w.flush_if_pending().unwrap();
        assert!(path.exists());
    }

    #[test]
    fn rotation_at_size_threshold_rolls_over() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Rabc-1000.jsonl");
        let mut w =
            JsonlBatchedWriter::open(&path, 512, false, rotating_every(4 * 1024, None)).unwrap();

        const RECORD_COUNT: u64 = 200;
        for sequence in 0..RECORD_COUNT {
            w.append_record(&numbered_record(sequence)).unwrap();
        }
        w.flush_and_fsync().unwrap();

        let names = segment_file_names(tmp.path());
        assert!(
            names.len() >= 2,
            "~19 KB against a 4 KB threshold must leave several segments, found {names:?}"
        );
        assert!(names.contains(&"Rabc-1000.jsonl".to_string()));
        assert!(names.contains(&"Rabc-1000.1.jsonl".to_string()));
        assert_eq!(
            sequences_across_segments(tmp.path()),
            (0..RECORD_COUNT).collect::<Vec<_>>(),
            "every record lands in exactly one segment"
        );
        for name in names
            .iter()
            .filter(|name| name.as_str() != "Rabc-1000.jsonl")
        {
            let rotated_bytes = std::fs::metadata(tmp.path().join(name)).unwrap().len();
            assert!(
                rotated_bytes >= 4 * 1024,
                "{name} rolled over at {rotated_bytes} bytes, before its threshold"
            );
        }
    }

    #[test]
    fn retention_deletes_oldest_segments() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Rabc-1000.jsonl");
        let record = numbered_record(0);
        // One record per flush, a threshold one record wide: every flush rotates.
        let mut w = JsonlBatchedWriter::open(
            &path,
            1,
            false,
            rotating_every(record.len() as u64 + 1, Some(5)),
        )
        .unwrap();

        for sequence in 0..12 {
            w.append_record(&numbered_record(sequence)).unwrap();
        }

        assert_eq!(
            segment_file_names(tmp.path()),
            [
                "Rabc-1000.10.jsonl",
                "Rabc-1000.11.jsonl",
                "Rabc-1000.12.jsonl",
                "Rabc-1000.9.jsonl",
                "Rabc-1000.jsonl",
            ],
            "twelve rotations keep the active segment and the four newest rotated ones"
        );
        assert_eq!(sequences_across_segments(tmp.path()), [8, 9, 10, 11]);
    }

    #[test]
    fn a_retention_of_one_keeps_only_the_active_segment() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Rabc-1000.jsonl");
        let mut w = JsonlBatchedWriter::open(&path, 1, false, rotating_every(1, Some(1))).unwrap();

        for sequence in 0..3 {
            w.append_record(&numbered_record(sequence)).unwrap();
        }

        assert_eq!(segment_file_names(tmp.path()), ["Rabc-1000.jsonl"]);
    }

    #[test]
    fn rotation_atomicity_no_split_records() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Rabc-1000.jsonl");
        let record_bytes = numbered_record(0).len() as u64 + 1;
        // Each batch holds seven records and the threshold is crossed partway
        // through the second, which must still land whole in one segment.
        let mut w = JsonlBatchedWriter::open(
            &path,
            (7 * record_bytes) as usize,
            false,
            rotating_every(10 * record_bytes, None),
        )
        .unwrap();

        const RECORD_COUNT: u64 = 50;
        for sequence in 0..RECORD_COUNT {
            w.append_record(&numbered_record(sequence)).unwrap();
        }
        w.flush_and_fsync().unwrap();

        assert!(segment_file_names(tmp.path()).len() >= 2);
        assert_eq!(
            sequences_across_segments(tmp.path()),
            (0..RECORD_COUNT).collect::<Vec<_>>()
        );
    }

    #[test]
    fn flush_and_fsync_never_rotates_the_active_segment() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Rabc-1000.jsonl");
        let mut w =
            JsonlBatchedWriter::open(&path, 1024 * 1024, false, rotating_every(1, None)).unwrap();

        w.append_record(&numbered_record(0)).unwrap();
        w.flush_and_fsync().unwrap();

        assert_eq!(segment_file_names(tmp.path()), ["Rabc-1000.jsonl"]);
        assert_eq!(sequences_across_segments(tmp.path()), [0]);
    }

    #[test]
    fn reopening_an_existing_segment_counts_its_bytes_toward_the_threshold() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Rabc-1000.jsonl");
        std::fs::write(&path, "x".repeat(4 * 1024)).unwrap();
        let mut w =
            JsonlBatchedWriter::open(&path, 1, false, rotating_every(4 * 1024, None)).unwrap();

        w.append_record(&numbered_record(0)).unwrap();

        assert_eq!(
            segment_file_names(tmp.path()),
            ["Rabc-1000.1.jsonl", "Rabc-1000.jsonl"]
        );
    }

    #[test]
    fn reopening_an_existing_segment_numbers_rotations_after_the_ones_already_on_disk() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Rabc-1000.jsonl");
        std::fs::write(tmp.path().join("Rabc-1000.1.jsonl"), "{\"sequence\":100}\n").unwrap();
        std::fs::write(tmp.path().join("Rabc-1000.2.jsonl"), "{\"sequence\":101}\n").unwrap();
        std::fs::write(
            tmp.path().join("Rabc-10000.7.jsonl"),
            "{\"sequence\":102}\n",
        )
        .unwrap();
        let mut w = JsonlBatchedWriter::open(&path, 1, false, rotating_every(1, None)).unwrap();

        w.append_record(&numbered_record(0)).unwrap();

        assert_eq!(
            segment_file_names(tmp.path()),
            [
                "Rabc-1000.1.jsonl",
                "Rabc-1000.2.jsonl",
                "Rabc-1000.3.jsonl",
                "Rabc-1000.jsonl",
                "Rabc-10000.7.jsonl",
            ],
            "the earlier run's segments survive and the new rotation takes the next number"
        );
    }

    #[test]
    fn a_rotated_segment_removed_from_outside_does_not_stop_retention() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Rabc-1000.jsonl");
        let mut w = JsonlBatchedWriter::open(&path, 1, false, rotating_every(1, Some(3))).unwrap();
        for sequence in 0..2 {
            w.append_record(&numbered_record(sequence)).unwrap();
        }
        std::fs::remove_file(tmp.path().join("Rabc-1000.1.jsonl")).unwrap();

        for sequence in 2..5 {
            w.append_record(&numbered_record(sequence))
                .expect("an already-missing expired segment is not a failure");
        }

        assert_eq!(
            segment_file_names(tmp.path()),
            ["Rabc-1000.4.jsonl", "Rabc-1000.5.jsonl", "Rabc-1000.jsonl"]
        );
    }

    #[test]
    fn a_rotation_leaves_no_replacement_file_behind() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Rabc-1000.jsonl");
        let mut w = JsonlBatchedWriter::open(&path, 1, false, rotating_every(1, None)).unwrap();

        w.append_record(&numbered_record(0)).unwrap();
        w.append_record(&numbered_record(1)).unwrap();

        assert_eq!(
            every_file_name(tmp.path()),
            ["Rabc-1000.1.jsonl", "Rabc-1000.2.jsonl", "Rabc-1000.jsonl"]
        );
    }

    #[test]
    fn a_rotation_that_cannot_create_its_replacement_leaves_the_active_name_in_place() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Rabc-1000.jsonl");
        let blocking_directory = tmp.path().join("Rabc-1000.jsonl.rotating");
        std::fs::create_dir(&blocking_directory).unwrap();
        let mut w = JsonlBatchedWriter::open(&path, 1, false, rotating_every(1, None)).unwrap();

        assert!(w.append_record(&numbered_record(0)).is_err());
        assert!(w.append_record(&numbered_record(1)).is_err());

        assert_eq!(
            segment_file_names(tmp.path()),
            ["Rabc-1000.jsonl"],
            "a rotation with nowhere to put its replacement must not move the active segment"
        );
        assert_eq!(sequences_across_segments(tmp.path()), [0, 1]);

        std::fs::remove_dir(&blocking_directory).unwrap();
        w.append_record(&numbered_record(2)).unwrap();

        assert_eq!(
            every_file_name(tmp.path()),
            ["Rabc-1000.1.jsonl", "Rabc-1000.jsonl"]
        );
        assert_eq!(sequences_across_segments(tmp.path()), [0, 1, 2]);
    }

    #[test]
    fn a_rotated_in_segment_truncated_from_outside_keeps_appending_at_its_end() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Rabc-1000.jsonl");
        let record_bytes = numbered_record(0).len() as u64 + 1;
        let mut w =
            JsonlBatchedWriter::open(&path, 1, false, rotating_every(3 * record_bytes, None))
                .unwrap();
        for sequence in 0..4 {
            w.append_record(&numbered_record(sequence)).unwrap();
        }
        assert!(tmp.path().join("Rabc-1000.1.jsonl").exists());

        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(0)
            .unwrap();
        w.append_record(&numbered_record(4)).unwrap();

        let active_contents = read_all(&path);
        assert_eq!(
            active_contents,
            format!("{}\n", String::from_utf8(numbered_record(4)).unwrap()),
            "a write past a truncation must land at the new end, not leave a hole before it"
        );
    }
}
