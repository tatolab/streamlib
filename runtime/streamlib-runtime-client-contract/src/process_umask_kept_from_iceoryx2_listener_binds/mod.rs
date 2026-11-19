// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Keeps the work that reads the process umask away from the iceoryx2
//! listener binds that change it.
//!
//! iceoryx2 binds each listener's unix datagram socket under a process-wide
//! `umask(!permission)`, 0o177 from 0.10, and restores it after `bind()`. A
//! directory made in that window has no owner search bit, so nothing can be
//! created in it. A child forked in it keeps 0o177 for its whole life. Every
//! listener bind, child start and directory creation of a runtime process takes
//! the one lock here, so none of them sees a bind's umask.
//!
//! A directory that needs an exact mode is made with
//! [`crate::directory_at_an_explicit_mode`] instead, which sets the mode after
//! `mkdir` and needs no lock.

use std::io;
use std::path::Path;
use std::sync::Mutex;

static PROCESS_UMASK_READ_OR_CHANGED: Mutex<()> = Mutex::new(());

/// Run `work` while no other thread of this process binds an iceoryx2
/// listener, starts a child, or makes a directory through this module.
///
/// An iceoryx2 listener bind runs inside this. So does every child start and
/// every directory made at the process umask. A panic in an earlier holder
/// leaves the umask as it was, so the lock is taken past it.
pub fn with_the_process_umask_held<T>(work: impl FnOnce() -> T) -> T {
    let _the_process_umask_is_held = PROCESS_UMASK_READ_OR_CHANGED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    work()
}

/// [`std::fs::create_dir_all`] at this process's own umask, never at the one an
/// iceoryx2 listener bind sets on another thread.
pub fn create_directory_and_its_missing_parents_at_the_process_umask(
    path: &Path,
) -> io::Result<()> {
    with_the_process_umask_held(|| std::fs::create_dir_all(path))
}

/// [`std::fs::create_dir`] at this process's own umask, never at the one an
/// iceoryx2 listener bind sets on another thread.
pub fn create_directory_at_the_process_umask(path: &Path) -> io::Result<()> {
    with_the_process_umask_held(|| std::fs::create_dir(path))
}

#[cfg(test)]
mod tests;
