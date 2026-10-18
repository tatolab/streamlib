// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What every child process the runtime starts is given before its exec: a
//! process group of its own, a lifetime bound to the runtime's, and no
//! descriptor beyond its standard streams.

use std::process::Command;

use crate::core::runtime::the_open_descriptor_limit;

/// Give the child its own process group and tie its lifetime to this process.
///
/// The process group is what keeps a terminal Ctrl-C from reaching children
/// directly: the signal goes to the app, and children come down through the
/// teardown the app then runs, having had a chance to release what they hold.
/// `PR_SET_PDEATHSIG` covers the case the app never gets to run teardown —
/// a `SIGKILL`ed parent leaves no orphans — and the `getppid` recheck closes
/// the window where the parent died between fork and that call, which would
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
/// higher than that.
///
/// A session's limit can be a million, and a million `fcntl` calls between fork
/// and exec would cost more than the interpreter start they precede. A
/// descriptor above this is the stated residual of the fallback arm.
const DESCRIPTOR_SWEEP_FALLBACK_CEILING: u64 = 65_536;

/// Give the child no descriptor beyond its standard streams.
///
/// `docs/plan/ARCHITECTURE.md` §Processor model: a helper inherits nothing but
/// its escalate socket and its standard streams. Everything else this process
/// holds — the stdio interceptor's four dups and two pipe read ends, another
/// helper's surface-share dups — reaches a child today, and a grandchild
/// holding one keeps the app's output open past the app's own exit. That is the
/// "won't quit even with SIGKILL" shape.
///
/// Marked close-on-exec rather than closed, because std's own machinery is
/// still using descriptors here: the pipe it reports a failed `exec` on is one
/// of them, and closing it would make a failure to start read as a success.
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
