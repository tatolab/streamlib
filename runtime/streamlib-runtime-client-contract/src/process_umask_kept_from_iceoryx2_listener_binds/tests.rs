// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Set only in the child process the umask test re-runs itself in.
const UMASK_CHILD_SCRATCH_DIRECTORY_ENVIRONMENT_VARIABLE: &str =
    "STREAMLIB_TEST_PROCESS_UMASK_CHILD_SCRATCH_DIRECTORY";

/// The umask iceoryx2 0.10 holds the whole process under while it binds a
/// listener's socket.
const UMASK_WHILE_ICEORYX2_BINDS_A_LISTENER: libc::mode_t = 0o177;

/// The child process's own umask, which every directory it makes must carry.
const THIS_PROCESSS_OWN_UMASK: libc::mode_t = 0o022;

const DIRECTORIES_MADE_BESIDE_THE_BINDS: usize = 400;

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Directories made while another thread swaps the umask to iceoryx2's bind
/// umask and back, as a listener bind does, all come out at this process's own
/// umask, nested levels included.
///
/// The umask is process-wide, so the child test process sets it and every test
/// running beside this one keeps its own. Modes are read rather than entries
/// written, so a run as root, which ignores the search bit, still tells the two
/// apart.
///
/// Fail-without-fix: make the directories with a plain `create_dir_all` and
/// some come out 0o600, because they were made inside a bind's window.
#[test]
fn a_directory_made_beside_listener_binds_carries_this_processs_own_umask() {
    if let Some(scratch_directory) =
        std::env::var_os(UMASK_CHILD_SCRATCH_DIRECTORY_ENVIRONMENT_VARIABLE)
    {
        let scratch_directory = Path::new(&scratch_directory).to_path_buf();
        // SAFETY: `umask` only swaps the process's file-creation mask; this
        // child process runs this one test and nothing else.
        unsafe { libc::umask(THIS_PROCESSS_OWN_UMASK) };

        let directories_are_still_being_made = Arc::new(AtomicBool::new(true));
        let binds = std::thread::spawn({
            let directories_are_still_being_made = Arc::clone(&directories_are_still_being_made);
            move || {
                while directories_are_still_being_made.load(Ordering::Relaxed) {
                    with_the_process_umask_held(|| {
                        // SAFETY: as iceoryx2's bind does: the umask is swapped
                        // and put back while the lock is held.
                        let own = unsafe { libc::umask(UMASK_WHILE_ICEORYX2_BINDS_A_LISTENER) };
                        std::thread::yield_now();
                        unsafe { libc::umask(own) };
                    });
                }
            }
        });

        let made: Vec<_> = (0..DIRECTORIES_MADE_BESIDE_THE_BINDS)
            .map(|index| {
                let nested = scratch_directory.join(format!("{index}")).join("nested");
                create_directory_and_its_missing_parents_at_the_process_umask(&nested).unwrap();
                let single = scratch_directory.join(format!("{index}-single"));
                create_directory_at_the_process_umask(&single).unwrap();
                [nested.parent().unwrap().to_path_buf(), nested, single]
            })
            .collect();
        directories_are_still_being_made.store(false, Ordering::Relaxed);
        binds.join().unwrap();

        for level in made.iter().flatten() {
            assert_eq!(mode_of(level), 0o755, "{}", level.display());
        }
        return;
    }

    let scratch_directory =
        crate::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
    let child = crate::test_support::rerun_this_test_in_a_child_process(
        "process_umask_kept_from_iceoryx2_listener_binds::tests::a_directory_made_beside_listener_binds_carries_this_processs_own_umask",
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
