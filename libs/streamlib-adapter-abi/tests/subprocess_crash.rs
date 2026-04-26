// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Self-test for the [`SubprocessCrashHarness`] primitive.
//!
//! Goal: prove that "subprocess holds a kernel fd → SIGKILL → kernel
//! closes the fd → host's epoll wakes" works end-to-end via the harness
//! without depending on streamlib's surface-share state.
//!
//! The end-to-end against the real surface-share watchdog will land
//! alongside that watchdog in a follow-up issue (see PR description).
//! This file exercises the harness primitive — that's enough to give
//! 3rd-party adapter authors confidence the API is shaped correctly.

#![cfg(target_os = "linux")]

use std::io::Read;
use std::os::unix::io::FromRawFd;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use streamlib_adapter_abi::testing::{CrashTiming, SubprocessCrashHarness};

/// Spawn a `sleep` subprocess that inherits a write end of a pipe; SIGKILL
/// it via the harness; verify the parent observes EOF on the read end
/// (kernel-FD-cleanup) within the cleanup budget.
#[test]
fn harness_signals_cleanup_after_kernel_closes_inherited_fd() {
    // Pipe: parent reads, child holds the write end open by inheriting it.
    let mut fds = [0i32; 2];
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "pipe() failed: {}", std::io::Error::last_os_error());
    let read_fd = fds[0];
    let write_fd = fds[1];

    // Set the read end to non-blocking so the observer closure can poll.
    let flags = unsafe { libc::fcntl(read_fd, libc::F_GETFL) };
    assert!(flags >= 0);
    let _ = unsafe { libc::fcntl(read_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };

    let mut cmd = Command::new("sleep");
    cmd.arg("60");
    // Inherit the write_fd into the child via pre_exec — keeping the
    // CLOEXEC flag clear ensures the kernel keeps it open across exec.
    use std::os::unix::process::CommandExt;
    let write_fd_for_child = write_fd;
    unsafe {
        cmd.pre_exec(move || {
            // Clear CLOEXEC if set (mirrors the surface-share check_out path
            // where the host hands a long-lived fd to the subprocess).
            let cloexec = libc::fcntl(write_fd_for_child, libc::F_GETFD);
            if cloexec >= 0 {
                let _ = libc::fcntl(
                    write_fd_for_child,
                    libc::F_SETFD,
                    cloexec & !libc::FD_CLOEXEC,
                );
            }
            Ok(())
        });
    }
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());

    // Close the parent's copy of write_fd AFTER spawn — for now keep it
    // open until after spawn so the test setup doesn't race.
    let cleanup_observed = Arc::new(AtomicBool::new(false));
    let observed = cleanup_observed.clone();

    let harness = SubprocessCrashHarness::new(cmd)
        .with_timing(CrashTiming::AfterDelay(Duration::from_millis(100)))
        .with_cleanup_timeout(Duration::from_secs(2))
        .with_cleanup_poll_interval(Duration::from_millis(20))
        // After spawn, the kernel has duplicated write_fd into the child
        // via fork. Close the parent's copy so the child becomes the
        // sole writer — kernel-FD-cleanup on SIGKILL then drops the
        // last reference and the read end sees EOF.
        .with_post_spawn(move |_child| {
            unsafe { libc::close(write_fd) };
            Ok(())
        });

    let outcome = harness
        .run(move || {
            // Try to read 1 byte. Three states:
            //   - 0 bytes: EOF — every writer closed the pipe → cleanup done
            //   - >0 bytes or non-EAGAIN error: should not happen for `sleep`
            //   - EAGAIN: still waiting
            let mut buf = [0u8; 1];
            // Use a duplicated FD so we don't accidentally close the
            // original via the File destructor on every poll.
            let dup_fd = unsafe { libc::dup(read_fd) };
            if dup_fd < 0 {
                return Err("dup failed");
            }
            let mut f = unsafe { std::fs::File::from_raw_fd(dup_fd) };
            match f.read(&mut buf) {
                Ok(0) => {
                    observed.store(true, Ordering::Release);
                    Ok(())
                }
                Ok(_) => Err("unexpected data on pipe"),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    Err("still waiting (EAGAIN)")
                }
                Err(_) => Err("unexpected I/O error"),
            }
            // f drops here, closing the duplicated fd; the original
            // read_fd remains owned by us.
        })
        .expect("harness must observe cleanup before timeout");

    assert!(
        cleanup_observed.load(Ordering::Acquire),
        "observer closure must have flagged cleanup"
    );
    assert!(
        outcome.cleanup_latency < Duration::from_secs(1),
        "kernel-FD-cleanup latency exceeded 1s: {:?}",
        outcome.cleanup_latency
    );

    // The post_spawn hook already closed write_fd; close our read end.
    unsafe {
        libc::close(read_fd);
    }
}

#[test]
fn harness_returns_timeout_error_when_cleanup_never_signals() {
    // No fd-passing — just `sleep 60`, observer returns Err forever.
    let mut cmd = Command::new("sleep");
    cmd.arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let harness = SubprocessCrashHarness::new(cmd)
        .with_timing(CrashTiming::AfterDelay(Duration::from_millis(20)))
        .with_cleanup_timeout(Duration::from_millis(150))
        .with_cleanup_poll_interval(Duration::from_millis(20));
    let err = harness
        .run(|| Err("never cleans up"))
        .expect_err("harness must time out when observer never reports cleanup");
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
}
