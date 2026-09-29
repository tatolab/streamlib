// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Directories that stay usable while iceoryx2 binds a listener on another
//! thread.
//!
//! iceoryx2 binds each listener's unix datagram socket under a process-wide
//! `umask(!permission)` it restores right after `bind()`, and from 0.10 that
//! permission is owner read-write only (`iceoryx2-bb-posix`
//! `UnixDatagramReceiver::bind`, `iceoryx2-cal` `SOCKET_PERMISSIONS`). A
//! directory any other thread of the process creates inside that window comes
//! out with a umask of 0o177 applied: no owner search bit, so nothing can be
//! created in it.

use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;

/// The owner's read, write and search bits.
const OWNER_READ_WRITE_AND_SEARCH_MODE_BITS: u32 = 0o700;

/// The mode `std::fs::create_dir_all` asks for, which the process umask then
/// narrows.
pub const ORDINARY_DIRECTORY_MODE_BEFORE_THE_UMASK: u32 = 0o777;

/// Create `path` and any missing parents at `mode`, then give each directory
/// this call created back whatever owner bits of `mode` a concurrent umask took.
///
/// Only the owner bits are restored: a umask never has a reason to take those,
/// while the group and other bits are the caller's umask to decide.
pub fn create_directory_and_parents_the_owner_can_enter(path: &Path, mode: u32) -> io::Result<()> {
    let directories_this_call_creates: Vec<&Path> = path
        .ancestors()
        .take_while(|ancestor| !ancestor.as_os_str().is_empty() && !ancestor.exists())
        .collect();
    let owner_bits_asked_for = mode & OWNER_READ_WRITE_AND_SEARCH_MODE_BITS;
    // One level at a time, each repaired before the next is made inside it: a
    // directory the umask left without its search bit cannot hold the next.
    for directory in directories_this_call_creates.into_iter().rev() {
        match std::fs::DirBuilder::new().mode(mode).create(directory) {
            Ok(()) => {}
            Err(failure) if failure.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(failure) => return Err(failure),
        }
        let mode_it_came_out_with = std::fs::metadata(directory)?.permissions().mode();
        if mode_it_came_out_with & owner_bits_asked_for != owner_bits_asked_for {
            std::fs::set_permissions(
                directory,
                std::fs::Permissions::from_mode(
                    (mode_it_came_out_with | owner_bits_asked_for) & 0o7777,
                ),
            )?;
        }
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

    fn owner_bits_of(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode()
            & OWNER_READ_WRITE_AND_SEARCH_MODE_BITS
    }

    /// Under the umask iceoryx2 binds a listener with, a plain `create_dir`
    /// leaves the directory without the owner's search bit — so a plain
    /// `create_dir_all` cannot even make a second level — and this one leaves
    /// every level it makes enterable.
    ///
    /// The umask is process-wide, so the child test process sets it and every
    /// test running beside this one keeps its own. Modes are read rather than
    /// entries written, so a run as root — which ignores the search bit — still
    /// tells the two apart.
    ///
    /// Fail-without-fix: skip the permission repair, or make it only once every
    /// level is created, and the nested level cannot be created at all.
    #[test]
    fn a_directory_created_under_iceoryx2s_bind_umask_is_still_one_the_owner_can_enter() {
        if let Some(scratch_directory) =
            std::env::var_os(UMASK_CHILD_SCRATCH_DIRECTORY_ENVIRONMENT_VARIABLE)
        {
            let scratch_directory = Path::new(&scratch_directory);
            // SAFETY: `umask` only swaps the process's file-creation mask; this
            // child process runs this one test and nothing else.
            unsafe { libc::umask(UMASK_WHILE_ICEORYX2_BINDS_A_LISTENER) };

            let plain = scratch_directory.join("plain");
            std::fs::create_dir(&plain).unwrap();
            let repaired = scratch_directory.join("repaired");
            create_directory_and_parents_the_owner_can_enter(&repaired.join("nested"), 0o777)
                .unwrap();

            assert_eq!(
                owner_bits_of(&plain),
                0o600,
                "the premise: the umask takes the search bit"
            );
            for directory in [&repaired, &repaired.join("nested")] {
                assert_eq!(owner_bits_of(directory), 0o700, "{}", directory.display());
            }
            std::fs::write(repaired.join("nested").join("an-entry"), b"").unwrap();
            return;
        }

        let scratch_directory =
            crate::core::test_support::a_temporary_directory_the_owner_can_enter().unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "iceoryx2::directory_the_owner_can_enter::tests::a_directory_created_under_iceoryx2s_bind_umask_is_still_one_the_owner_can_enter",
                "--exact",
                "--test-threads=1",
            ])
            .env(
                UMASK_CHILD_SCRATCH_DIRECTORY_ENVIRONMENT_VARIABLE,
                scratch_directory.path(),
            )
            .output()
            .expect("the test binary re-runs this test in a child process");
        let child_output = String::from_utf8_lossy(&child.stdout);
        assert!(
            child.status.success() && child_output.contains("1 passed"),
            "the child must run the umask arm and pass: {child_output}{}",
            String::from_utf8_lossy(&child.stderr),
        );
        // The child's plain directory cannot be entered, so the scratch
        // directory could not otherwise be removed.
        std::fs::set_permissions(
            scratch_directory.path().join("plain"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }

    /// A requested mode tighter than the default is kept to the bit: an
    /// owner-only directory stays owner-only.
    #[test]
    fn an_owner_only_directory_comes_out_owner_only() {
        let scratch_directory =
            crate::core::test_support::a_temporary_directory_the_owner_can_enter().unwrap();
        let owner_only = scratch_directory.path().join("owner-only");

        create_directory_and_parents_the_owner_can_enter(&owner_only, 0o700).unwrap();

        assert_eq!(
            std::fs::metadata(&owner_only).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    /// A directory that was already there is left exactly as it was, even when
    /// its owner cannot enter it: it is not this call's to change.
    #[test]
    fn a_directory_that_already_existed_is_left_as_it_was() {
        let scratch_directory =
            crate::core::test_support::a_temporary_directory_the_owner_can_enter().unwrap();
        let already_there = scratch_directory.path().join("already-there");
        std::fs::create_dir(&already_there).unwrap();
        std::fs::set_permissions(&already_there, std::fs::Permissions::from_mode(0o500)).unwrap();

        create_directory_and_parents_the_owner_can_enter(&already_there, 0o777).unwrap();

        assert_eq!(
            std::fs::metadata(&already_there)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o500
        );
        std::fs::set_permissions(&already_there, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}
