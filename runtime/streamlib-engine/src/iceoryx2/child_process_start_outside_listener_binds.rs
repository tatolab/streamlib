// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Keeps a child process from being started while iceoryx2 binds a listener.
//!
//! iceoryx2 binds each listener's unix datagram socket under a process-wide
//! `umask(!permission)` — 0o177 from 0.10 — and restores it after `bind()`. A
//! child forked inside that window inherits 0o177 and keeps it for its whole
//! life, so every directory it goes on to create has no owner search bit.
//! Unlike a directory, a child's umask cannot be set after the fact to a value
//! the parent does not know, so the two are kept apart instead.

use iceoryx2::port::listener::{Listener, ListenerCreateError};
use iceoryx2::prelude::*;
use iceoryx2::service::port_factory::event::PortFactory as EventServicePortFactory;

/// Held for the length of every iceoryx2 listener bind and every child
/// process start in this process.
static ICEORYX2_LISTENER_BIND_OR_CHILD_PROCESS_START: parking_lot::Mutex<()> =
    parking_lot::const_mutex(());

/// Start a child process while no iceoryx2 listener is being bound in this
/// process, so the child inherits this process's own umask.
pub fn start_a_child_process_outside_every_iceoryx2_listener_bind<ChildProcessStart>(
    start_the_child_process: impl FnOnce() -> ChildProcessStart,
) -> ChildProcessStart {
    let _no_listener_is_being_bound = ICEORYX2_LISTENER_BIND_OR_CHILD_PROCESS_START.lock();
    start_the_child_process()
}

/// Bind a listener on `notify_service` while no child process is being
/// started in this process.
pub(crate) fn bind_an_iceoryx2_listener_outside_every_child_process_start(
    notify_service: &EventServicePortFactory<ipc::Service>,
) -> Result<Listener<ipc::Service>, ListenerCreateError> {
    let _no_child_process_is_starting = ICEORYX2_LISTENER_BIND_OR_CHILD_PROCESS_START.lock();
    notify_service.listener_builder().create()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// This process's umask as Linux reports it, read without changing it.
    fn this_processs_umask() -> String {
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("Umask:"))
            .expect("Linux reports the umask in /proc/self/status")
            .trim()
            .to_string()
    }

    /// Children started while another thread binds listener after listener all
    /// inherit this process's own umask, never the one iceoryx2 binds under.
    ///
    /// Fail-without-fix: start the children without the lock and some report
    /// `0177` — 3 to 5 of the 200 in each of three runs on the rig. How many
    /// depends on how the binds and the starts interleave, so a revert can pass
    /// a single run; the fixed form cannot produce one at all.
    #[test]
    fn a_child_started_beside_listener_binds_inherits_this_processs_own_umask() {
        let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
        let notify_service = node
            .service_builder(
                &ServiceName::new(&format!(
                    "test/child-process-umask/{}",
                    crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix(
                    )
                ))
                .unwrap(),
            )
            .event()
            .max_listeners(1)
            .open_or_create()
            .unwrap();
        let umask_this_process_runs_under = this_processs_umask();

        let keep_binding = Arc::new(AtomicBool::new(true));
        let binder = {
            let keep_binding = Arc::clone(&keep_binding);
            std::thread::spawn(move || {
                while keep_binding.load(Ordering::Relaxed) {
                    drop(
                        bind_an_iceoryx2_listener_outside_every_child_process_start(
                            &notify_service,
                        )
                        .unwrap(),
                    );
                }
            })
        };

        let umasks_the_children_reported: Vec<String> = (0..200)
            .map(|_| {
                let child = start_a_child_process_outside_every_iceoryx2_listener_bind(|| {
                    std::process::Command::new("sh")
                        .args(["-c", "umask"])
                        .output()
                })
                .unwrap();
                String::from_utf8_lossy(&child.stdout).trim().to_string()
            })
            .collect();
        keep_binding.store(false, Ordering::Relaxed);
        binder.join().unwrap();

        let children_under_another_umask = umasks_the_children_reported
            .iter()
            .filter(|umask| **umask != umask_this_process_runs_under)
            .count();
        assert_eq!(
            children_under_another_umask, 0,
            "{children_under_another_umask} of 200 children did not inherit this process's umask \
             {umask_this_process_runs_under}: {umasks_the_children_reported:?}"
        );
    }
}
