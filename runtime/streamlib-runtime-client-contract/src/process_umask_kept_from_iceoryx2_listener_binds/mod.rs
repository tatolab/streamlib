// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Keeps the work that reads the process umask away from the iceoryx2
//! listener binds that change it.
//!
//! iceoryx2 binds each listener's unix datagram socket under a process-wide
//! `umask(!permission)`, 0o177 from 0.10, and restores it after `bind()`. A
//! folder made in that window has no owner search bit, so nothing can be
//! created in it, and a child forked in it keeps 0o177 for its whole life. The
//! runtime's listener binds, child starts and folders made at the process umask
//! take the one lock here; the `no-directory-made-beside-a-listener-bind` rule
//! keeps a plain `create_dir` out of runtime code. A folder that needs an exact
//! mode is made with [`crate::directory_at_an_explicit_mode`] instead.

use std::io;
use std::path::Path;
use std::sync::Mutex;

static PROCESS_UMASK_READ_OR_CHANGED: Mutex<()> = Mutex::new(());

/// Run `work` while no other thread of this process holds this lock; `work`
/// must not call back into this module, or it deadlocks.
pub fn with_no_iceoryx2_listener_bind_changing_the_umask<T>(work: impl FnOnce() -> T) -> T {
    // The lock guards no data, so a holder that panicked leaves nothing to repair.
    let _no_bind_changes_the_umask = PROCESS_UMASK_READ_OR_CHANGED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    work()
}

/// [`std::fs::create_dir_all`] at this process's own umask, never at the one an
/// iceoryx2 listener bind sets on another thread.
pub fn create_directory_and_its_missing_parents_at_the_process_umask(
    path: &Path,
) -> io::Result<()> {
    with_no_iceoryx2_listener_bind_changing_the_umask(|| std::fs::create_dir_all(path))
}

/// [`std::fs::create_dir`] at this process's own umask, never at the one an
/// iceoryx2 listener bind sets on another thread.
pub fn create_directory_at_the_process_umask(path: &Path) -> io::Result<()> {
    with_no_iceoryx2_listener_bind_changing_the_umask(|| std::fs::create_dir(path))
}

#[cfg(test)]
mod tests;
