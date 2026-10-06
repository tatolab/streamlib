// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Clearing the path a runtime is about to bind one of its Unix sockets at.
//!
//! A runtime that crashed leaves its socket file behind, and binding over it
//! fails. A file nothing answers on is that leftover and is removed; a file a
//! live process answers on belongs to a second runtime with the same id, which
//! is refused rather than displaced.

use std::io;
use std::path::{Path, PathBuf};

/// What stood at a Unix socket path that is now clear to bind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnixSocketPathClearedForBind {
    /// Nothing was at the path.
    NothingWasThere,
    /// A file no process answers on was at the path, and has been removed.
    StaleSocketFileRemoved,
}

/// Why a Unix socket path cannot be bound.
#[derive(Debug, thiserror::Error)]
pub enum UnixSocketPathRefusedForBind {
    /// A live process answers a connect on the path.
    #[error("{} is already bound by a live process", path.display())]
    HeldByALiveProcess { path: PathBuf },
    /// No process answers on the path, and the file could not be removed.
    #[error("found a stale socket {} that no process answers on, but failed to remove it: {source}", path.display())]
    StaleSocketFileNotRemoved { path: PathBuf, source: io::Error },
}

/// Probe `path` with a connect: refuse it when a live process answers, remove
/// the file when nothing does.
pub fn clear_unix_socket_path_for_bind(
    path: &Path,
) -> Result<UnixSocketPathClearedForBind, UnixSocketPathRefusedForBind> {
    // `symlink_metadata`, not `exists`: a dangling symlink at the path still
    // makes the bind fail, so it is probed and cleared like any other file.
    if std::fs::symlink_metadata(path).is_err() {
        return Ok(UnixSocketPathClearedForBind::NothingWasThere);
    }
    if std::os::unix::net::UnixStream::connect(path).is_ok() {
        return Err(UnixSocketPathRefusedForBind::HeldByALiveProcess {
            path: path.to_path_buf(),
        });
    }
    std::fs::remove_file(path).map_err(|source| {
        UnixSocketPathRefusedForBind::StaleSocketFileNotRemoved {
            path: path.to_path_buf(),
            source,
        }
    })?;
    Ok(UnixSocketPathClearedForBind::StaleSocketFileRemoved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_path_is_clear_to_bind() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nothing.sock");

        assert_eq!(
            clear_unix_socket_path_for_bind(&path).unwrap(),
            UnixSocketPathClearedForBind::NothingWasThere
        );
    }

    #[test]
    fn a_socket_file_nothing_answers_on_is_removed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("stale.sock");
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists(), "dropping a listener leaves its file behind");

        assert_eq!(
            clear_unix_socket_path_for_bind(&path).unwrap(),
            UnixSocketPathClearedForBind::StaleSocketFileRemoved
        );
        assert!(!path.exists());
        std::os::unix::net::UnixListener::bind(&path).expect("the cleared path binds");
    }

    #[test]
    fn a_socket_a_live_process_answers_on_is_refused_naming_the_path_and_left_alone() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("live.sock");
        let _live_listener = std::os::unix::net::UnixListener::bind(&path).unwrap();

        let refusal = clear_unix_socket_path_for_bind(&path).unwrap_err();

        assert!(matches!(
            refusal,
            UnixSocketPathRefusedForBind::HeldByALiveProcess { .. }
        ));
        assert!(
            refusal.to_string().contains(&path.display().to_string()),
            "{refusal}"
        );
        assert!(path.exists(), "a live process's socket must not be removed");
    }

    #[test]
    fn a_dangling_symlink_at_the_path_is_cleared() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dangling.sock");
        std::os::unix::fs::symlink(directory.path().join("gone"), &path).unwrap();

        assert_eq!(
            clear_unix_socket_path_for_bind(&path).unwrap(),
            UnixSocketPathClearedForBind::StaleSocketFileRemoved
        );
        assert!(std::fs::symlink_metadata(&path).is_err());
    }
}
