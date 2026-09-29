// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Directories created at an explicit mode, never at whatever the process
//! umask happens to be.
//!
//! iceoryx2 binds each listener's unix datagram socket under a process-wide
//! `umask(!permission)`, and from 0.10 that permission is owner read-write only
//! (`iceoryx2-bb-posix` `UnixDatagramReceiver::bind`, `iceoryx2-cal`
//! `SOCKET_PERMISSIONS`). A directory any other thread creates inside that
//! window gets a umask of 0o177 applied. iceoryx2's own `Directory::create`
//! does not leave its directories to the umask for that reason: it applies the
//! intended permission after `mkdir`. This is the same rule for ours.

use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;

/// A directory only its owner can list, create entries in, or enter.
pub const OWNER_ONLY_DIRECTORY_MODE: u32 = 0o700;

/// Create `path` and any missing parents, each at exactly `mode` whatever the
/// process umask is at that instant.
///
/// One level at a time, each given its mode before the next is made inside it.
/// A level that already existed is left as it is.
pub fn create_directory_and_its_missing_parents_at_mode(path: &Path, mode: u32) -> io::Result<()> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    let levels_this_call_creates: Vec<&Path> = path
        .ancestors()
        .take_while(|ancestor| !ancestor.as_os_str().is_empty() && !ancestor.exists())
        .collect();
    for level in levels_this_call_creates.into_iter().rev() {
        match std::fs::DirBuilder::new().mode(mode).create(level) {
            Ok(()) => {}
            Err(failure) if failure.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(failure) => return Err(failure),
        }
        std::fs::set_permissions(level, std::fs::Permissions::from_mode(mode))?;
    }
    if path.is_dir() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} exists and is not a directory", path.display()),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Set only in the child process the umask test re-runs itself in.
    const UMASK_CHILD_SCRATCH_DIRECTORY_ENVIRONMENT_VARIABLE: &str =
        "STREAMLIB_TEST_UMASK_CHILD_SCRATCH_DIRECTORY";

    /// The umask iceoryx2 0.10 holds the whole process under while it binds a
    /// listener's socket.
    const UMASK_WHILE_ICEORYX2_BINDS_A_LISTENER: libc::mode_t = 0o177;

    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// Under the umask iceoryx2 binds a listener with, a plain `create_dir`
    /// comes out 0o600 — no owner search bit, so a plain `create_dir_all`
    /// cannot even make a second level — while every level this creates comes
    /// out at exactly the mode asked for, group bits included.
    ///
    /// The umask is process-wide, so the child test process sets it and every
    /// test running beside this one keeps its own. Modes are read rather than
    /// entries written, so a run as root — which ignores the search bit — still
    /// tells the two apart.
    ///
    /// Fail-without-fix: leave the mode to the umask, or apply it only once
    /// every level is created, and the nested level cannot be created at all.
    #[test]
    fn a_directory_created_under_iceoryx2s_bind_umask_comes_out_at_the_mode_asked_for() {
        if let Some(scratch_directory) =
            std::env::var_os(UMASK_CHILD_SCRATCH_DIRECTORY_ENVIRONMENT_VARIABLE)
        {
            let scratch_directory = Path::new(&scratch_directory);
            // SAFETY: `umask` only swaps the process's file-creation mask; this
            // child process runs this one test and nothing else.
            unsafe { libc::umask(UMASK_WHILE_ICEORYX2_BINDS_A_LISTENER) };

            let plain = scratch_directory.join("plain");
            std::fs::create_dir(&plain).unwrap();
            assert_eq!(
                mode_of(&plain),
                0o600,
                "the premise: the umask takes the search bit"
            );

            for (tree, mode) in [
                ("owner-only", OWNER_ONLY_DIRECTORY_MODE),
                ("group-readable", 0o750),
            ] {
                let nested = scratch_directory.join(tree).join("nested");
                create_directory_and_its_missing_parents_at_mode(&nested, mode).unwrap();
                for level in [nested.parent().unwrap(), nested.as_path()] {
                    assert_eq!(mode_of(level), mode, "{}", level.display());
                }
                std::fs::write(nested.join("an-entry"), b"").unwrap();
            }
            return;
        }

        let scratch_directory =
            crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
        let child = crate::core::test_support::rerun_this_test_in_a_child_process(
            "core::directory_at_an_explicit_mode::tests::a_directory_created_under_iceoryx2s_bind_umask_comes_out_at_the_mode_asked_for",
            UMASK_CHILD_SCRATCH_DIRECTORY_ENVIRONMENT_VARIABLE,
            scratch_directory.path().as_os_str(),
        );
        assert!(
            child.status.success(),
            "the umask arm failed in the child: {}{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr),
        );
    }

    /// A directory that was already there is left exactly as it was, even when
    /// its owner cannot enter it: it is not this call's to change.
    #[test]
    fn a_directory_that_already_existed_is_left_as_it_was() {
        let scratch_directory =
            crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
        let already_there = scratch_directory.path().join("already-there");
        std::fs::create_dir(&already_there).unwrap();
        std::fs::set_permissions(&already_there, std::fs::Permissions::from_mode(0o500)).unwrap();

        create_directory_and_its_missing_parents_at_mode(&already_there, OWNER_ONLY_DIRECTORY_MODE)
            .unwrap();

        assert_eq!(mode_of(&already_there), 0o500);
        std::fs::set_permissions(
            &already_there,
            std::fs::Permissions::from_mode(OWNER_ONLY_DIRECTORY_MODE),
        )
        .unwrap();
    }
}
