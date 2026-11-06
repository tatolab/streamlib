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
    pidfd_unless_it_is_this_process: Option<OwnedFd>,
}

impl GpuPoolClaimantProcess {
    /// Hold the process `pid` names while it runs: by a pidfd, unless it is
    /// this process, which is never stopped. Refused when the process cannot
    /// be held or has already exited, since its column could then never clear.
    pub fn hold_the_peer(pid: u32) -> Result<Self, String> {
        if pid == std::process::id() {
            return Ok(Self {
                claimant: GpuPoolClaimantProcessId {
                    pid,
                    incarnation_of_the_pid: incarnation_of_the_pid_of(pid)?,
                },
                pidfd_unless_it_is_this_process: None,
            });
        }
        // SAFETY: pidfd_open takes a pid and a flags word and answers a fresh
        // fd or -1.
        let raw_pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
        if raw_pidfd < 0 {
            return Err(format!(
                "claimant process {pid} could not be held by a pidfd ({}), so its claim table \
                 column could never be cleared",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: pidfd_open answered a fresh fd nothing else owns.
        let pidfd = unsafe { OwnedFd::from_raw_fd(raw_pidfd as libc::c_int) };
        let incarnation_of_the_pid = incarnation_of_the_pid_of(pid)?;
        let claimant_process = Self {
            claimant: GpuPoolClaimantProcessId {
                pid,
                incarnation_of_the_pid,
            },
            pidfd_unless_it_is_this_process: Some(pidfd),
        };
        if claimant_process.has_exited_within(0) {
            return Err(format!(
                "claimant process {pid} exited before it could be handed a column"
            ));
        }
        Ok(claimant_process)
    }

    /// The claimant this process claims as.
    pub fn claimant(&self) -> GpuPoolClaimantProcessId {
        self.claimant
    }

    /// Stop the process: the service dropped its connection, and a claimant
    /// left running could still read a slot whose column was cleared.
    pub fn kill(&self) {
        let Some(pidfd) = &self.pidfd_unless_it_is_this_process else {
            return;
        };
        // SAFETY: a valid pidfd, a signal number, no siginfo and no flags.
        let signalled = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                pidfd.as_raw_fd(),
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
                    self.claimant.pid
                );
            }
        }
    }

    /// Block until the process has exited, answering whether it has; an
    /// exited process runs nothing, so nothing it claimed is still read. This
    /// process never exits under the service, so it answers `false`.
    pub fn wait_until_it_has_exited(&self) -> bool {
        self.pidfd_unless_it_is_this_process.is_some() && self.has_exited_within(-1)
    }

    fn has_exited_within(&self, timeout_ms: libc::c_int) -> bool {
        let Some(pidfd) = &self.pidfd_unless_it_is_this_process else {
            return false;
        };
        let mut exit_readiness = libc::pollfd {
            fd: pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            // SAFETY: one valid pollfd for the call's duration.
            let ready = unsafe { libc::poll(&mut exit_readiness, 1, timeout_ms) };
            if ready > 0 {
                return true;
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
    pub fn clear_its_columns_once_it_has_exited(
        self,
        claim_tables: &GpuPoolClaimTables,
        the_service_dropped_its_connection: bool,
    ) {
        if self.pidfd_unless_it_is_this_process.is_none() {
            return;
        }
        if the_service_dropped_its_connection {
            self.kill();
        }
        if !self.wait_until_it_has_exited() {
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

/// Field 22 of `/proc/<pid>/stat`: when the process started, in clock ticks
/// since boot. Fields are counted after the parenthesised command name, which
/// may itself hold spaces and parentheses.
fn incarnation_of_the_pid_of(pid: u32) -> Result<u64, String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map_err(|failure| format!("reading /proc/{pid}/stat failed: {failure}"))?;
    incarnation_of_the_pid_in_a_proc_stat_line(&stat)
        .ok_or_else(|| format!("/proc/{pid}/stat states no start time"))
}

fn incarnation_of_the_pid_in_a_proc_stat_line(stat: &str) -> Option<u64> {
    const FIELDS_AFTER_THE_COMMAND_NAME_BEFORE_THE_START_TIME: usize = 19;
    let (_, after_the_command_name) = stat.rsplit_once(')')?;
    after_the_command_name
        .split_ascii_whitespace()
        .nth(FIELDS_AFTER_THE_COMMAND_NAME_BEFORE_THE_START_TIME)?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_held_but_never_stopped_or_waited_on() {
        let this_process = GpuPoolClaimantProcess::hold_the_peer(std::process::id()).unwrap();

        this_process.kill();

        assert!(!this_process.wait_until_it_has_exited());
        assert!(this_process.claimant().incarnation_of_the_pid > 0);
    }

    #[test]
    fn a_dropped_claimant_is_stopped_and_waited_on() {
        let mut sleeper = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("a child to stop");
        let claimant = GpuPoolClaimantProcess::hold_the_peer(sleeper.id())
            .expect("a live child is held by a pidfd");

        claimant.kill();

        assert!(claimant.wait_until_it_has_exited());
        let exit = sleeper.wait().expect("the child is reaped");
        assert!(!exit.success(), "the child was stopped, not left to finish");
    }

    #[test]
    fn a_pid_no_process_holds_is_refused() {
        let mut finished = std::process::Command::new("true")
            .spawn()
            .expect("a child that exits");
        let pid = finished.id();
        finished.wait().expect("the child is reaped");

        assert!(GpuPoolClaimantProcess::hold_the_peer(pid).is_err());
    }

    #[test]
    fn the_start_time_is_read_past_a_command_name_holding_spaces_and_parentheses() {
        let stat = "4242 (a (weird) name) S 1 4242 4242 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 \
                    987654 1000 50";

        assert_eq!(
            incarnation_of_the_pid_in_a_proc_stat_line(stat),
            Some(987_654)
        );
    }
}
