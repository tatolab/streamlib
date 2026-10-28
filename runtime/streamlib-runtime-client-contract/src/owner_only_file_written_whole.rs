// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A file only its owner reads, replaced whole or not at all: the stream
//! records, `relays.json` and the relay secrets' fallback file.

use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Only the owner reads or writes the file.
pub const OWNER_ONLY_FILE_MODE: u32 = 0o600;

/// Write `contents` to `path` whole or not at all: to an owner-only temporary
/// file beside it — hidden, `.<name>.<pid>.<n>.tmp` — synced, then renamed over
/// it, its directory synced after, best effort.
pub fn write_an_owner_only_file_whole(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let temporary_path = a_temporary_path_beside(path);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(OWNER_ONLY_FILE_MODE)
        .open(&temporary_path)
        .and_then(|mut temporary_file| {
            temporary_file.write_all(contents)?;
            temporary_file.sync_all()
        })
        .and_then(|()| std::fs::rename(&temporary_path, path));
    if let Err(write_failure) = written {
        let _ = std::fs::remove_file(&temporary_path);
        return Err(write_failure);
    }
    if let Some(directory) = path.parent()
        && let Ok(directory) = std::fs::File::open(directory)
    {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn a_temporary_path_beside(path: &Path) -> PathBuf {
    static TEMPORARY_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);
    let file_name = path
        .file_name()
        .map(|file_name| file_name.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        TEMPORARY_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_file_is_replaced_whole_and_its_owners_alone() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("record.json");
        write_an_owner_only_file_whole(&path, b"first").unwrap();
        write_an_owner_only_file_whole(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            OWNER_ONLY_FILE_MODE
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
