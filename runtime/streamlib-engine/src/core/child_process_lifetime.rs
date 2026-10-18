// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What every child process the runtime starts is given before its exec — a
//! process group of its own, a lifetime bound to the runtime's, no descriptor
//! beyond its standard streams — and how its exit is seen without freeing its
//! pid.

use std::process::Command;

use crate::core::runtime::the_open_descriptor_limit;

/// Give the child its own process group, out of a terminal Ctrl-C's reach, and
/// on Linux a `SIGKILL` when this process dies. The `getppid` recheck catches
/// a parent that died between the fork and `PR_SET_PDEATHSIG`, which would
/// otherwise arm a signal that never fires.
pub fn detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    let spawning_process_id = std::process::id() as libc::pid_t;
    // SAFETY: everything called here is async-signal-safe, which is the
    // contract for a `pre_exec` closure running between fork and exec.
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            #[cfg(target_os = "linux")]
            {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != spawning_process_id {
                    libc::_exit(1);
                }
            }
            Ok(())
        });
    }
}

/// The first descriptor past standard input, output and error.
const FIRST_DESCRIPTOR_PAST_STDIO: libc::c_uint = 3;

/// How far the per-descriptor fallback walks when the process's own limit is
/// higher than that; a descriptor above it is left as it is.
const DESCRIPTOR_SWEEP_FALLBACK_CEILING: u64 = 65_536;

/// Give the child no descriptor beyond its standard streams. Each is marked
/// close-on-exec rather than closed: std reports a failed `exec` over a pipe
/// it still holds here, and closing it would make a failure read as a start.
pub fn give_the_child_no_descriptor_beyond_stdio(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    // Read here rather than in the child: `getrlimit` is not on POSIX's
    // async-signal-safe list, and the answer cannot change for the child
    // between this call and its exec.
    let highest_descriptor_the_fallback_walks = highest_descriptor_the_fallback_walks();

    let sweep_every_descriptor_past_stdio = move || {
        if one_syscall_marked_every_descriptor_past_stdio_close_on_exec() {
            return Ok(());
        }
        // SAFETY: this closure is the `pre_exec` the call below registers, and
        // `fcntl` is all the fallback reaches for.
        unsafe {
            mark_each_descriptor_past_stdio_close_on_exec(highest_descriptor_the_fallback_walks)
        };
        Ok(())
    };
    // SAFETY: the closure calls only the `close_range` syscall and `fcntl`,
    // both async-signal-safe, which is the contract for a `pre_exec` closure
    // running between fork and exec.
    unsafe {
        command.pre_exec(sweep_every_descriptor_past_stdio);
    }
}

/// How far the per-descriptor fallback walks in this process, read before the
/// fork so the child never has to ask.
fn highest_descriptor_the_fallback_walks() -> libc::c_uint {
    // Clamped in the wide type: `rlim_cur` is 64-bit and `RLIM_INFINITY` is its
    // maximum, so narrowing first would wrap a large real limit down to a small
    // one and stop the sweep short.
    the_open_descriptor_limit().map_or(DESCRIPTOR_SWEEP_FALLBACK_CEILING, |descriptor_limit| {
        descriptor_limit
            .rlim_cur
            .min(DESCRIPTOR_SWEEP_FALLBACK_CEILING)
    }) as libc::c_uint
}

/// Ask the kernel to mark the whole range at once, and say whether it did.
///
/// The raw syscall rather than glibc's `close_range` wrapper: that symbol
/// arrived in glibc 2.34, and the release wheel is linked inside
/// `manylinux_2_28`, whose glibc is 2.28 — an extern reference would fail to
/// link there while every CI runner, on a newer glibc, links it happily. The
/// syscall itself needs only Linux 5.9, and 5.11 for the flag.
#[cfg(target_os = "linux")]
fn one_syscall_marked_every_descriptor_past_stdio_close_on_exec() -> bool {
    // SAFETY: a raw syscall with scalar arguments, async-signal-safe.
    let swept = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            libc::c_long::from(FIRST_DESCRIPTOR_PAST_STDIO),
            libc::c_long::from(libc::c_uint::MAX),
            libc::c_long::from(libc::CLOSE_RANGE_CLOEXEC),
        )
    };
    swept == 0
}

/// macOS has no `close_range`, so its children always take the fallback.
#[cfg(not(target_os = "linux"))]
fn one_syscall_marked_every_descriptor_past_stdio_close_on_exec() -> bool {
    false
}

/// Mark each descriptor past stdio close-on-exec, one at a time.
///
/// Reached on macOS always, and on a Linux kernel older than the
/// `CLOSE_RANGE_CLOEXEC` flag.
///
/// # Safety
///
/// Called only from a `pre_exec` closure. `fcntl` is on POSIX's
/// async-signal-safe list, which is what makes that legal.
unsafe fn mark_each_descriptor_past_stdio_close_on_exec(highest_descriptor: libc::c_uint) {
    for descriptor in FIRST_DESCRIPTOR_PAST_STDIO..highest_descriptor {
        let flags = unsafe { libc::fcntl(descriptor as libc::c_int, libc::F_GETFD) };
        if flags < 0 {
            continue;
        }
        unsafe {
            libc::fcntl(
                descriptor as libc::c_int,
                libc::F_SETFD,
                flags | libc::FD_CLOEXEC,
            )
        };
    }
}

/// Whether child `process_id` has exited, leaving it collectable but not
/// collected: the zombie keeps its pid, and the process group id equal to it,
/// from being handed to another process before a signal that follows.
pub fn a_child_process_has_exited_without_being_reaped(process_id: u32) -> bool {
    // SAFETY: a zeroed `siginfo_t` is a valid buffer for `waitid` to fill, and
    // the call reports rather than collects.
    let mut reported: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `reported` is a valid, live `siginfo_t` for the duration.
    let waited = unsafe {
        libc::waitid(
            libc::P_PID,
            process_id as libc::id_t,
            &mut reported,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    // `WNOHANG` returns 0 with nothing reported while the child is still
    // running, and the zeroed pid is how that case is told from a real one.
    waited == 0 && reported_process_id(&reported) == process_id as libc::pid_t
}

#[cfg(target_os = "linux")]
fn reported_process_id(reported: &libc::siginfo_t) -> libc::pid_t {
    // SAFETY: `si_pid` reads the union arm a `SIGCHLD`-shaped report fills.
    unsafe { reported.si_pid() }
}

#[cfg(not(target_os = "linux"))]
fn reported_process_id(reported: &libc::siginfo_t) -> libc::pid_t {
    reported.si_pid
}
