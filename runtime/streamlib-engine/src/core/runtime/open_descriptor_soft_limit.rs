// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The runtime's soft open-descriptor limit: raised to its ceiling for the
//! runtime itself, and put back for every interpreter a stream starts.
//!
//! A service manager starts the runtime at a soft limit of 1024 (systemd) or 256
//! (launchd), which a handful of streams exhaust. The interpreters go back to
//! the limit the runtime started with, because a program that still calls
//! `select()` cannot watch a descriptor at or past `FD_SETSIZE` (1024).

use std::fmt;
use std::process::Command;
use std::sync::OnceLock;

/// The open-descriptor limit this process started with, recorded by the raise.
static OPEN_DESCRIPTOR_LIMIT_THIS_PROCESS_STARTED_WITH: OnceLock<libc::rlimit> = OnceLock::new();

/// What [`raise_the_open_descriptor_soft_limit_to_its_ceiling`] did.
#[derive(Debug)]
pub enum OpenDescriptorSoftLimitRaise {
    /// The soft limit moved from `started_with` to `raised_to`.
    Raised {
        started_with: libc::rlim_t,
        raised_to: libc::rlim_t,
    },
    /// The soft limit was already at its ceiling.
    AlreadyAtItsCeiling { soft_limit: libc::rlim_t },
    /// The limit could not be read or set; the process runs at the limit it
    /// started with.
    Refused { refusal: std::io::Error },
}

impl fmt::Display for OpenDescriptorSoftLimitRaise {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Raised {
                started_with,
                raised_to,
            } => write!(
                formatter,
                "the runtime's open-descriptor soft limit was raised from {started_with} to \
                 {raised_to}; the interpreters it starts run at {started_with}"
            ),
            Self::AlreadyAtItsCeiling { soft_limit } => write!(
                formatter,
                "the runtime's open-descriptor soft limit is already at its ceiling, {soft_limit}"
            ),
            Self::Refused { refusal } => write!(
                formatter,
                "the runtime's open-descriptor soft limit could not be raised, so it runs at the \
                 limit it started with: {refusal}"
            ),
        }
    }
}

/// Raise this process's soft open-descriptor limit to the highest value it
/// may take.
///
/// The first call records the limit the process started with, which
/// [`start_the_interpreter_at_the_open_descriptor_soft_limit_the_runtime_started_with`]
/// hands every interpreter; a later call never changes that record.
#[must_use]
pub fn raise_the_open_descriptor_soft_limit_to_its_ceiling() -> OpenDescriptorSoftLimitRaise {
    let current = match the_open_descriptor_limit() {
        Ok(open_descriptor_limit) => open_descriptor_limit,
        Err(refusal) => return OpenDescriptorSoftLimitRaise::Refused { refusal },
    };
    // Recorded before the raise, so no interpreter started meanwhile inherits
    // the raised limit.
    let started_with = *OPEN_DESCRIPTOR_LIMIT_THIS_PROCESS_STARTED_WITH.get_or_init(|| current);
    let ceiling = match the_highest_soft_limit_this_process_may_take(current.rlim_max) {
        Ok(ceiling) => ceiling,
        Err(refusal) => return OpenDescriptorSoftLimitRaise::Refused { refusal },
    };
    if current.rlim_cur >= ceiling {
        return OpenDescriptorSoftLimitRaise::AlreadyAtItsCeiling {
            soft_limit: current.rlim_cur,
        };
    }
    let raised = libc::rlimit {
        rlim_cur: ceiling,
        rlim_max: current.rlim_max,
    };
    // SAFETY: `raised` is a valid, live `rlimit`.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) } != 0 {
        return OpenDescriptorSoftLimitRaise::Refused {
            refusal: std::io::Error::last_os_error(),
        };
    }
    OpenDescriptorSoftLimitRaise::Raised {
        started_with: started_with.rlim_cur,
        raised_to: ceiling,
    }
}

/// Have `command`'s child start at the open-descriptor limit this process
/// started with, once the raise has recorded it; a no-op before.
pub(crate) fn start_the_interpreter_at_the_open_descriptor_soft_limit_the_runtime_started_with(
    command: &mut Command,
) {
    use std::os::unix::process::CommandExt;

    let Some(&started_with) = OPEN_DESCRIPTOR_LIMIT_THIS_PROCESS_STARTED_WITH.get() else {
        return;
    };
    let lower_the_soft_limit = move || {
        // SAFETY: `setrlimit` with a valid `rlimit` is one system call.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &started_with) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    };
    // SAFETY: a `pre_exec` closure must be async-signal-safe. `setrlimit` is not
    // on POSIX's list, but glibc, musl and libSystem each implement it as a bare
    // system call that neither locks nor allocates, and `last_os_error` only
    // reads errno.
    unsafe {
        command.pre_exec(lower_the_soft_limit);
    }
}

/// This process's open-descriptor limit, soft and hard.
pub(crate) fn the_open_descriptor_limit() -> std::io::Result<libc::rlimit> {
    // SAFETY: a zeroed `rlimit` is a valid buffer for `getrlimit` to fill.
    let mut open_descriptor_limit: libc::rlimit = unsafe { std::mem::zeroed() };
    // SAFETY: `open_descriptor_limit` is a valid, live `rlimit` for the duration.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut open_descriptor_limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(open_descriptor_limit)
}

#[cfg(not(target_os = "macos"))]
fn the_highest_soft_limit_this_process_may_take(
    hard_limit: libc::rlim_t,
) -> std::io::Result<libc::rlim_t> {
    Ok(hard_limit)
}

/// macOS refuses a soft limit above `kern.maxfilesperproc`, and its hard limit
/// is usually `RLIM_INFINITY`.
#[cfg(target_os = "macos")]
fn the_highest_soft_limit_this_process_may_take(
    hard_limit: libc::rlim_t,
) -> std::io::Result<libc::rlim_t> {
    let mut descriptors_per_process: libc::c_int = 0;
    let mut answer_length = std::mem::size_of::<libc::c_int>();
    // SAFETY: the name is NUL-terminated, and the answer buffer and its length
    // are valid for the duration.
    let sysctl_status = unsafe {
        libc::sysctlbyname(
            c"kern.maxfilesperproc".as_ptr(),
            std::ptr::from_mut(&mut descriptors_per_process).cast(),
            &mut answer_length,
            std::ptr::null_mut(),
            0,
        )
    };
    if sysctl_status != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let descriptors_per_process = libc::rlim_t::try_from(descriptors_per_process)
        .map_err(|_| std::io::Error::other("kern.maxfilesperproc is negative"))?;
    Ok(hard_limit.min(descriptors_per_process))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::compiler::compiler_ops::processor_interpreter_spawn_host::stream_interpreter_command_in_its_project_directory;

    /// Set in the child process the test re-runs itself in.
    const RAISE_IN_THIS_PROCESS_ENVIRONMENT_VARIABLE: &str =
        "STREAMLIB_TEST_RAISE_THE_OPEN_DESCRIPTOR_SOFT_LIMIT_IN_THIS_PROCESS";

    /// The soft limit the child starts at, as a service manager would start
    /// the runtime.
    const SOFT_LIMIT_A_SERVICE_MANAGER_STARTS_THE_RUNTIME_AT: libc::rlim_t = 256;

    fn soft_limit_an_interpreter_the_runtime_starts_reports() -> libc::rlim_t {
        let project_directory = tempfile::tempdir().expect("a project directory");
        let output = stream_interpreter_command_in_its_project_directory(
            std::path::Path::new("/bin/sh"),
            project_directory.path(),
            std::path::Path::new("/opt/tatolab/lib/tatolab/lend"),
        )
        .expect("the test directories hold no path-list separator")
        .args(["-c", "ulimit -Sn"])
        .output()
        .expect("the shell starts");
        String::from_utf8(output.stdout)
            .expect("the shell answers in UTF-8")
            .trim()
            .parse()
            .expect("`ulimit -Sn` prints a number")
    }

    fn raise_from_a_service_managers_limit_and_check_what_an_interpreter_inherits() {
        let limit_at_the_start = the_open_descriptor_limit().expect("the limit reads");
        let ceiling = the_highest_soft_limit_this_process_may_take(limit_at_the_start.rlim_max)
            .expect("the ceiling reads");
        assert!(
            ceiling > SOFT_LIMIT_A_SERVICE_MANAGER_STARTS_THE_RUNTIME_AT,
            "this machine's ceiling, {ceiling}, leaves nothing to raise"
        );
        let as_a_service_manager_starts_it = libc::rlimit {
            rlim_cur: SOFT_LIMIT_A_SERVICE_MANAGER_STARTS_THE_RUNTIME_AT,
            rlim_max: limit_at_the_start.rlim_max,
        };
        // SAFETY: a valid, live `rlimit`, set in a process this test owns.
        assert_eq!(
            unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &as_a_service_manager_starts_it) },
            0
        );

        let raise = raise_the_open_descriptor_soft_limit_to_its_ceiling();

        assert!(
            matches!(
                raise,
                OpenDescriptorSoftLimitRaise::Raised { started_with, raised_to }
                    if started_with == SOFT_LIMIT_A_SERVICE_MANAGER_STARTS_THE_RUNTIME_AT
                        && raised_to == ceiling
            ),
            "{raise}"
        );
        assert_eq!(
            the_open_descriptor_limit()
                .expect("the limit reads")
                .rlim_cur,
            ceiling
        );
        assert_eq!(
            soft_limit_an_interpreter_the_runtime_starts_reports(),
            SOFT_LIMIT_A_SERVICE_MANAGER_STARTS_THE_RUNTIME_AT
        );
        let second_raise = raise_the_open_descriptor_soft_limit_to_its_ceiling();
        assert!(
            matches!(
                second_raise,
                OpenDescriptorSoftLimitRaise::AlreadyAtItsCeiling { soft_limit }
                    if soft_limit == ceiling
            ),
            "{second_raise}"
        );
    }

    /// Run in a child process, because the raise is process-wide and outlives
    /// the test.
    #[test]
    fn the_raise_reaches_the_ceiling_and_an_interpreter_the_runtime_starts_runs_at_the_limit_it_started_with()
     {
        if std::env::var_os(RAISE_IN_THIS_PROCESS_ENVIRONMENT_VARIABLE).is_some() {
            raise_from_a_service_managers_limit_and_check_what_an_interpreter_inherits();
            return;
        }

        let child = crate::core::test_support::rerun_this_test_in_a_child_process(
            "core::runtime::open_descriptor_soft_limit::tests::the_raise_reaches_the_ceiling_and_an_interpreter_the_runtime_starts_runs_at_the_limit_it_started_with",
            RAISE_IN_THIS_PROCESS_ENVIRONMENT_VARIABLE,
            std::ffi::OsStr::new("1"),
        );

        assert!(
            child.status.success(),
            "the raise failed in the child process: {}\n{}\n{}",
            child.status,
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr),
        );
    }

    #[test]
    fn an_interpreter_started_before_any_raise_keeps_the_runtimes_own_limit() {
        assert!(
            OPEN_DESCRIPTOR_LIMIT_THIS_PROCESS_STARTED_WITH
                .get()
                .is_none(),
            "only the child process the raise test re-runs in may raise"
        );
        assert_eq!(
            soft_limit_an_interpreter_the_runtime_starts_reports(),
            the_open_descriptor_limit()
                .expect("the limit reads")
                .rlim_cur
        );
    }
}
