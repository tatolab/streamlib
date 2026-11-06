// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The process a claim table column was handed to, watched by a kqueue
//! process note so its column is cleared only once that very process has
//! exited, never a later process that reuses its pid.

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};

use streamlib_surface_client::SurfaceShareMachSenderAuditIdentity;

use crate::core::context::{GpuPoolClaimTables, GpuPoolClaimantProcessId};

/// A process handed a claimant column, which the service can stop and wait on.
pub(crate) struct GpuPoolClaimantProcess {
    claimant: GpuPoolClaimantProcessId,
    exit_watch_unless_it_is_this_process: Option<OwnedFd>,
    /// Set once the watch reports the exit: the kernel delivers that note
    /// once, so a later wait must not ask the kqueue again.
    exit_was_seen: AtomicBool,
}

impl GpuPoolClaimantProcess {
    /// Hold the process that sent with `sender`'s audit identity while it
    /// runs: by a kqueue watching for its exit, unless it is this process,
    /// which is never stopped. Refused when the process cannot be watched or
    /// has already exited, since its column could then never clear.
    pub(crate) fn hold_the_peer(
        sender: SurfaceShareMachSenderAuditIdentity,
    ) -> Result<Self, String> {
        let claimant = GpuPoolClaimantProcessId {
            pid: sender.pid as u32,
            incarnation_of_the_pid: sender.pidversion as u32 as u64,
        };
        if sender.pid as u32 == std::process::id() {
            return Ok(Self {
                claimant,
                exit_watch_unless_it_is_this_process: None,
                exit_was_seen: AtomicBool::new(false),
            });
        }
        // SAFETY: kqueue takes no arguments and answers a fresh fd or -1.
        let raw_kqueue = unsafe { libc::kqueue() };
        if raw_kqueue < 0 {
            return Err(format!(
                "claimant process {} could not be watched by a kqueue ({}), so its claim table \
                 column could never be cleared",
                sender.pid,
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: kqueue answered a fresh fd nothing else owns.
        let exit_watch = unsafe { OwnedFd::from_raw_fd(raw_kqueue) };
        let watch_for_its_exit = libc::kevent {
            ident: sender.pid as libc::uintptr_t,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: one valid change, no event list, no timeout.
        let registered = unsafe {
            libc::kevent(
                exit_watch.as_raw_fd(),
                &watch_for_its_exit,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if registered < 0 {
            return Err(format!(
                "claimant process {} could not be watched for its exit ({}), so its claim table \
                 column could never be cleared",
                sender.pid,
                std::io::Error::last_os_error()
            ));
        }
        Ok(Self {
            claimant,
            exit_watch_unless_it_is_this_process: Some(exit_watch),
            exit_was_seen: AtomicBool::new(false),
        })
    }

    /// The claimant this process claims as.
    pub(crate) fn claimant(&self) -> GpuPoolClaimantProcessId {
        self.claimant
    }

    /// Stop the process: the service dropped its connection, and a claimant
    /// left running could still read a slot whose column was cleared. A
    /// process whose exit was not yet seen still holds its pid, as a zombie
    /// at least, so the signal reaches no other process.
    fn kill(&self) {
        if self.exit_watch_unless_it_is_this_process.is_none() || self.has_exited_within(Some(0)) {
            return;
        }
        // SAFETY: a pid and a signal number.
        if unsafe { libc::kill(self.claimant.pid as libc::pid_t, libc::SIGKILL) } != 0 {
            let failure = std::io::Error::last_os_error();
            if failure.raw_os_error() != Some(libc::ESRCH) {
                tracing::warn!(
                    "[Surface share] stopping claimant process {} failed: {failure}",
                    self.claimant.pid
                );
            }
        }
    }

    fn has_exited_within(&self, timeout_seconds: Option<i64>) -> bool {
        let Some(exit_watch) = &self.exit_watch_unless_it_is_this_process else {
            return false;
        };
        if self.exit_was_seen.load(Ordering::Acquire) {
            return true;
        }
        let timeout = timeout_seconds.map(|seconds| libc::timespec {
            tv_sec: seconds as libc::time_t,
            tv_nsec: 0,
        });
        loop {
            // SAFETY: an all-zero kevent is a valid out-parameter.
            let mut seen: libc::kevent = unsafe { std::mem::zeroed() };
            // SAFETY: no changes, one out-event, and an optional timeout that
            // outlives the call.
            let ready = unsafe {
                libc::kevent(
                    exit_watch.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    &mut seen,
                    1,
                    timeout
                        .as_ref()
                        .map_or(std::ptr::null(), |timeout| timeout as *const libc::timespec),
                )
            };
            if ready > 0 {
                if seen.fflags & libc::NOTE_EXIT != 0 {
                    self.exit_was_seen.store(true, Ordering::Release);
                    return true;
                }
                continue;
            }
            if ready == 0 {
                return false;
            }
            let failure = std::io::Error::last_os_error();
            if failure.kind() != std::io::ErrorKind::Interrupted {
                tracing::warn!(
                    "[Surface share] waiting on claimant process {} failed: {failure}",
                    self.claimant.pid
                );
                return false;
            }
        }
    }

    /// Once the process has exited, clear its column in every claim table,
    /// stopping it first when the service dropped its connection. A process
    /// not seen to exit keeps its column.
    pub(crate) fn clear_its_columns_once_it_has_exited(
        self,
        claim_tables: &GpuPoolClaimTables,
        the_service_dropped_its_connection: bool,
    ) {
        if self.exit_watch_unless_it_is_this_process.is_none() {
            return;
        }
        if the_service_dropped_its_connection {
            self.kill();
        }
        if !self.has_exited_within(None) {
            tracing::warn!(
                "[Surface share] claimant process {} was not seen to exit; its claim table \
                 columns stay held",
                self.claimant.pid
            );
            return;
        }
        let cleared_tables = claim_tables.clear_every_column_of_a_reaped_claimant(self.claimant);
        tracing::debug!(
            "[Surface share] claimant process {} exited; its column cleared in {cleared_tables} \
             claim tables",
            self.claimant.pid
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_child_handed_a_column_is_seen_to_exit_once_stopped() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("a child process starts");
        let claimant_process =
            GpuPoolClaimantProcess::hold_the_peer(SurfaceShareMachSenderAuditIdentity {
                pid: child.id() as libc::pid_t,
                pidversion: 1,
            })
            .expect("a running child is held");
        assert!(!claimant_process.has_exited_within(Some(0)));

        claimant_process.kill();

        assert!(claimant_process.has_exited_within(Some(10)));
        assert!(
            claimant_process.has_exited_within(None),
            "an exit already seen is not waited for again"
        );
        child.wait().expect("the child is reaped");
    }

    #[test]
    fn this_process_is_held_and_never_stopped() {
        let this_process =
            GpuPoolClaimantProcess::hold_the_peer(SurfaceShareMachSenderAuditIdentity {
                pid: std::process::id() as libc::pid_t,
                pidversion: 7,
            })
            .expect("this process is held");

        assert_eq!(this_process.claimant().incarnation_of_the_pid, 7);
        assert!(!this_process.has_exited_within(Some(0)));
    }
}
