// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The helper process groups a third interrupt kills before the app exits.
//!
//! `docs/plan/ARCHITECTURE.md` §Language SDKs: the third interrupt kills every
//! helper's process group and exits at once. It is read from the
//! signal-forwarding thread while any other thread may be wedged holding any
//! lock, so the registry takes none.
//!
//! A helper registers its group once it has started and leaves the registry
//! after the last signal its shutdown ladder sends and before its group leader
//! is reaped: a reaped leader's pid, and the group id equal to it, are free for
//! the OS to hand to someone else. A third interrupt landing between that
//! departure and the kill it was about to send is the stated residual, and it
//! can only reach a group the ladder has already killed.

use std::sync::atomic::{AtomicI32, Ordering};

/// How many helper process groups can be registered at once.
const HELPER_PROCESS_GROUP_REGISTRY_CAPACITY: usize = 1024;

/// A free slot. Never a registrable id: `killpg(0, …)` signals the caller's
/// own process group, which is the app itself.
const FREE_HELPER_PROCESS_GROUP_SLOT: i32 = 0;

/// Whether `process_group_id` could be a helper's own group.
///
/// Never zero, one or a negative, and never the app's own group: a third
/// interrupt killing that would take the shell job the app runs in with it —
/// `python app.py | tee log` included.
fn is_a_registrable_helper_process_group_id(process_group_id: i32) -> bool {
    // SAFETY: `getpgrp` takes no arguments and cannot fail.
    process_group_id > 1 && process_group_id != unsafe { libc::getpgrp() }
}

static REGISTERED_HELPER_PROCESS_GROUP_IDS: [AtomicI32; HELPER_PROCESS_GROUP_REGISTRY_CAPACITY] =
    [const { AtomicI32::new(FREE_HELPER_PROCESS_GROUP_SLOT) };
        HELPER_PROCESS_GROUP_REGISTRY_CAPACITY];

/// Register a helper's process group for the third interrupt to kill, and say
/// whether it was taken.
///
/// Refused for an id that cannot name a helper's own group, and when every slot
/// is taken.
#[must_use = "a refused registration leaves the group out of the third interrupt's kill"]
pub fn register_a_helper_process_group(process_group_id: i32) -> bool {
    if !is_a_registrable_helper_process_group_id(process_group_id) {
        return false;
    }
    REGISTERED_HELPER_PROCESS_GROUP_IDS.iter().any(|slot| {
        slot.compare_exchange(
            FREE_HELPER_PROCESS_GROUP_SLOT,
            process_group_id,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_ok()
    })
}

/// Take a helper's process group out of the registry. A group that was never
/// registered is left alone.
pub fn deregister_a_helper_process_group(process_group_id: i32) {
    if !is_a_registrable_helper_process_group_id(process_group_id) {
        return;
    }
    for slot in &REGISTERED_HELPER_PROCESS_GROUP_IDS {
        if slot
            .compare_exchange(
                process_group_id,
                FREE_HELPER_PROCESS_GROUP_SLOT,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
        {
            return;
        }
    }
}

/// `SIGKILL` every registered helper process group, returning how many were
/// signalled.
pub(crate) fn kill_every_registered_helper_process_group() -> usize {
    let mut signalled = 0usize;
    for slot in &REGISTERED_HELPER_PROCESS_GROUP_IDS {
        let process_group_id = slot.load(Ordering::SeqCst);
        if process_group_id == FREE_HELPER_PROCESS_GROUP_SLOT {
            continue;
        }
        // SAFETY: a scalar syscall. The id was registered as a helper's own
        // group and is taken out before that group's leader is reaped.
        unsafe { libc::killpg(process_group_id, libc::SIGKILL) };
        signalled += 1;
    }
    signalled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_support::a_process_parked_in_a_process_group_of_its_own;
    use serial_test::serial;
    use std::time::{Duration, Instant};

    /// Leaves nothing registered however the test ends.
    struct EveryRegisteredHelperProcessGroupDeregisteredOnDrop(Vec<i32>);

    impl Drop for EveryRegisteredHelperProcessGroupDeregisteredOnDrop {
        fn drop(&mut self) {
            for process_group_id in &self.0 {
                deregister_a_helper_process_group(*process_group_id);
            }
        }
    }

    fn has_exited_within(child: &mut std::process::Child, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        loop {
            if child.try_wait().expect("the child is waitable").is_some() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    #[serial]
    fn a_registered_group_is_killed_and_one_that_left_the_registry_is_not() {
        let mut registered = a_process_parked_in_a_process_group_of_its_own();
        let mut departed = a_process_parked_in_a_process_group_of_its_own();
        let registered_group = registered.id() as i32;
        let departed_group = departed.id() as i32;
        let _cleanup = EveryRegisteredHelperProcessGroupDeregisteredOnDrop(vec![
            registered_group,
            departed_group,
        ]);

        assert!(register_a_helper_process_group(registered_group));
        assert!(register_a_helper_process_group(departed_group));
        deregister_a_helper_process_group(departed_group);

        assert_eq!(kill_every_registered_helper_process_group(), 1);

        assert!(
            has_exited_within(&mut registered, Duration::from_secs(5)),
            "a registered helper process group survived the kill"
        );
        assert!(
            !has_exited_within(&mut departed, Duration::from_millis(200)),
            "a group that had left the registry was killed anyway"
        );
        let _ = departed.kill();
        let _ = departed.wait();
    }

    /// Fail-without-fix: registering zero makes the third interrupt
    /// `killpg(0, SIGKILL)`, and registering the app's own group id does the
    /// same by name — either kills the app's whole shell job.
    #[test]
    #[serial]
    fn no_id_that_could_name_the_apps_own_group_is_registered() {
        // SAFETY: `getpgrp` takes no arguments and cannot fail.
        let the_apps_own_group = unsafe { libc::getpgrp() };
        for unregistrable_id in [0, 1, -1, -4242, the_apps_own_group] {
            assert!(
                !register_a_helper_process_group(unregistrable_id),
                "{unregistrable_id} was registered"
            );
        }
    }

    #[test]
    #[serial]
    fn a_full_registry_refuses_rather_than_overwriting_a_group() {
        // Ids far above any live pid, never signalled here.
        let first_fake_id = 1_000_000_000;
        let fake_ids: Vec<i32> = (0..HELPER_PROCESS_GROUP_REGISTRY_CAPACITY as i32)
            .map(|offset| first_fake_id + offset)
            .collect();
        let _cleanup = EveryRegisteredHelperProcessGroupDeregisteredOnDrop(fake_ids.clone());

        for fake_id in &fake_ids {
            assert!(register_a_helper_process_group(*fake_id));
        }
        assert!(!register_a_helper_process_group(first_fake_id - 1));
        assert!(
            REGISTERED_HELPER_PROCESS_GROUP_IDS
                .iter()
                .all(|slot| fake_ids.contains(&slot.load(Ordering::SeqCst))),
            "a refused registration overwrote a group that was already there"
        );
    }
}
