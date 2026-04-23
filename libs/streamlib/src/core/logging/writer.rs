// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Batched append-only JSONL writer. Size + time flush; final `fdatasync`
//! on clean shutdown only.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

pub(crate) struct JsonlBatchedWriter {
    file: File,
    buffer: Vec<u8>,
    batch_bytes: usize,
    fsync_on_every_batch: bool,
}

impl JsonlBatchedWriter {
    pub fn open(path: &Path, batch_bytes: usize, fsync_on_every_batch: bool) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            file,
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
    /// durability contract.
    pub fn flush_and_fsync(&mut self) -> io::Result<()> {
        self.flush_buffer()?;
        self.file.sync_data()
    }

    fn flush_buffer(&mut self) -> io::Result<()> {
        // The buffer is newline-aligned by construction — every
        // `append_record` appends `bytes + '\n'` as a unit, so the buffer
        // always ends on a newline boundary and contains only whole
        // records.
        self.file.write_all(&self.buffer)?;
        self.buffer.clear();
        if self.fsync_on_every_batch {
            self.file.sync_data()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn read_all(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn records_accumulate_until_size_flush() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("out.jsonl");
        let mut w = JsonlBatchedWriter::open(&path, 32, false).unwrap();

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
        let mut w = JsonlBatchedWriter::open(&path, 1024 * 1024, false).unwrap();
        w.append_record(b"{\"a\":1}").unwrap();
        assert_eq!(read_all(&path), "");
        w.flush_if_pending().unwrap();
        assert_eq!(read_all(&path), "{\"a\":1}\n");
    }

    #[test]
    fn flush_and_fsync_persists() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("out.jsonl");
        let mut w = JsonlBatchedWriter::open(&path, 1024 * 1024, false).unwrap();
        w.append_record(b"{\"a\":1}").unwrap();
        w.flush_and_fsync().unwrap();
        assert_eq!(read_all(&path), "{\"a\":1}\n");
    }

    #[test]
    fn append_opens_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nested").join("dirs").join("out.jsonl");
        let mut w = JsonlBatchedWriter::open(&path, 1024 * 1024, false).unwrap();
        w.append_record(b"{}").unwrap();
        w.flush_if_pending().unwrap();
        assert!(path.exists());
    }
}
