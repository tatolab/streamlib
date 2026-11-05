// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The process a claim table column was handed to, held by a pidfd so its
//! column is cleared only once that very process has exited, never a later
//! process that reuses its pid.

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

use crate::core::context::{GpuPoolClaimTables, GpuPoolClaimantProcessId};

/// A process handed a claimant column, which the service can stop and wait on.
pub struct GpuPoolClaimantProcess {
    claimant: GpuPoolClaimantProcessId,
    pidfd: OwnedFd,
}

impl GpuPoolClaimantProcess {
    /// The process `claimant` names, while it is still running; `None` for
    /// this process, which is never stopped, or a pid no process holds.
    pub fn of_a_live_peer(claimant: GpuPoolClaimantProcessId) -> Option<Self> {
        if claimant.0 == std::process::id() {
            return None;
        }
        // SAFETY: pidfd_open takes a pid and a flags word and answers a fresh
        // fd or -1.
        let raw_pidfd =
            unsafe { libc::syscall(libc::SYS_pidfd_open, claimant.0 as libc::pid_t, 0) };
        if raw_pidfd < 0 {
            tracing::warn!(
                "[Surface share] claimant process {} could not be held by a pidfd ({}); its \
                 claim table column is not cleared when it exits",
                claimant.0,
                std::io::Error::last_os_error()
            );
            return None;
        }
        Some(Self {
            claimant,
            // SAFETY: pidfd_open answered a fresh fd nothing else owns.
            pidfd: unsafe { OwnedFd::from_raw_fd(raw_pidfd as libc::c_int) },
        })
    }

    /// Stop the process: the service dropped its connection, and a claimant
    /// left running could still read a slot whose column was cleared.
    pub fn kill(&self) {
        // SAFETY: a valid pidfd, a signal number, no siginfo and no flags.
        let signalled = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if signalled != 0 {
            let failure = std::io::Error::last_os_error();
            if failure.raw_os_error() != Some(libc::ESRCH) {
                tracing::warn!(
                    "[Surface share] stopping claimant process {} failed: {failure}",
                    self.claimant.0
                );
            }
        }
    }

    /// Block until the process has exited; an exited process runs nothing,
    /// so nothing it claimed is still read.
    pub fn wait_until_it_has_exited(&self) {
        let mut exit_readiness = libc::pollfd {
            fd: self.pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            // SAFETY: one valid pollfd for the call's duration.
            let ready = unsafe { libc::poll(&mut exit_readiness, 1, -1) };
            if ready > 0 {
                return;
            }
            if ready < 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
            {
                tracing::warn!(
                    "[Surface share] waiting on claimant process {} failed: {}",
                    self.claimant.0,
                    std::io::Error::last_os_error()
                );
                return;
            }
        }
    }

    /// Once the process has exited, clear its column in every claim table,
    /// stopping it first when the service dropped its connection.
    pub fn clear_its_columns_once_it_has_exited(
        self,
        claim_tables: &GpuPoolClaimTables,
        the_service_dropped_its_connection: bool,
    ) {
        if the_service_dropped_its_connection {
            self.kill();
        }
        self.wait_until_it_has_exited();
        let cleared_tables = claim_tables.clear_every_column_of_a_reaped_claimant(self.claimant);
        tracing::debug!(
            "[Surface share] claimant process {} exited; its column cleared in {cleared_tables} \
             claim tables",
            self.claimant.0
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_never_held_as_a_claimant_to_stop() {
        assert!(
            GpuPoolClaimantProcess::of_a_live_peer(GpuPoolClaimantProcessId(std::process::id()))
                .is_none()
        );
    }

    #[test]
    fn a_dropped_claimant_is_stopped_and_waited_on() {
        let mut sleeper = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("a child to stop");
        let claimant =
            GpuPoolClaimantProcess::of_a_live_peer(GpuPoolClaimantProcessId(sleeper.id()))
                .expect("a live child is held by a pidfd");

        claimant.kill();
        claimant.wait_until_it_has_exited();

        let exit = sleeper.wait().expect("the child is reaped");
        assert!(!exit.success(), "the child was stopped, not left to finish");
    }
}
