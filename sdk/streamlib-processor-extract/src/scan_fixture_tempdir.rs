// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one throwaway-package fixture every test module in this crate builds
//! its scan input with. Hand-rolled rather than pulled from `tempfile`: this
//! crate is deliberately lean (`syn` + `toml` + the schema types), and a scan
//! fixture needs only "a directory that deletes itself".

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A directory removed when the binding goes out of scope.
pub(crate) struct ScanFixtureTempDir(PathBuf);

impl ScanFixtureTempDir {
    /// The fixture root every relative path in a test is written under.
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScanFixtureTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A fresh fixture directory, named for the test module that asked so a leaked
/// directory names its owner.
pub(crate) fn scan_fixture_tempdir_named(prefix: &str) -> ScanFixtureTempDir {
    static SCAN_FIXTURE_TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = SCAN_FIXTURE_TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("{prefix}-{pid}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    ScanFixtureTempDir(dir)
}

/// Write one source file into the fixture, creating parent directories.
pub(crate) fn write_scan_fixture_file(dir: &Path, relative_path: &str, body: &str) {
    let path = dir.join(relative_path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}
