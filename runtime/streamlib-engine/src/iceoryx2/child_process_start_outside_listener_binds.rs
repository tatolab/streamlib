// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Keeps a child process from being started while iceoryx2 binds a listener.
//!
//! iceoryx2 binds each listener's unix datagram socket under a process-wide
//! `umask(!permission)`, 0o177 from 0.10, and restores it after `bind()`. A
//! child forked inside that window inherits 0o177 and keeps it for its whole
//! life, so every directory it goes on to create has no owner search bit.
//! Unlike a directory, a child's umask cannot be set after the fact to a value
//! the parent does not know, so the two are kept apart by the one lock in
//! [`streamlib_runtime_client_contract::process_umask_kept_from_iceoryx2_listener_binds`].

use iceoryx2::port::listener::{Listener, ListenerCreateError};
use iceoryx2::prelude::*;
use iceoryx2::service::port_factory::event::PortFactory as EventServicePortFactory;
use streamlib_runtime_client_contract::process_umask_kept_from_iceoryx2_listener_binds::with_the_process_umask_held;

/// Spawn `command` while no iceoryx2 listener is being bound in this process,
/// so the child inherits this process's own umask. The lock covers the spawn
/// alone: the umask is inherited at the fork, and `spawn` returns once the exec
/// has happened.
pub fn spawn_outside_every_iceoryx2_listener_bind(
    command: &mut std::process::Command,
) -> std::io::Result<std::process::Child> {
    with_the_process_umask_held(|| command.spawn())
}

/// Bind a listener on `notify_service` while no child process is being
/// started, and no directory made at the process umask, in this process.
pub(crate) fn bind_an_iceoryx2_listener_outside_every_child_process_start(
    notify_service: &EventServicePortFactory<ipc_threadsafe::Service>,
) -> Result<Listener<ipc_threadsafe::Service>, ListenerCreateError> {
    with_the_process_umask_held(|| notify_service.listener_builder().create())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// This process's own umask, read while no listener can be binding, as
    /// `sh -c umask` prints one.
    fn this_processs_own_umask() -> String {
        // SAFETY: `umask` only swaps the process's file-creation mask, and it
        // is put straight back; the lock keeps every listener bind and child
        // start away from the moment it reads 0.
        let umask = with_the_process_umask_held(|| unsafe {
            let umask = libc::umask(0);
            libc::umask(umask);
            umask
        });
        format!("{umask:04o}")
    }

    /// Children started while another thread binds listener after listener
    /// all inherit this process's own umask, never the one iceoryx2 binds
    /// under.
    ///
    /// Fail-without-fix: spawn the children without the lock and some report
    /// `0177` — between 1 and 8 of the 200 per run on the rig. How many depends
    /// on how the binds and the starts interleave, so a revert can pass a single
    /// run; the fixed form cannot produce one at all.
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

        let umask_this_process_runs_under = this_processs_own_umask();

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
                let child = spawn_outside_every_iceoryx2_listener_bind(
                    std::process::Command::new("sh")
                        .args(["-c", "umask"])
                        .stdout(std::process::Stdio::piped()),
                )
                .unwrap();
                let output = child.wait_with_output().unwrap();
                String::from_utf8_lossy(&output.stdout).trim().to_string()
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
