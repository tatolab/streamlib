// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Subprocess SIGABRTs mid-write. Verifies that:
//!  1. The host adapter's per-surface state survives the crash.
//!  2. The host can still acquire write/read on the same surface after
//!     the crash, because the subprocess's in-process VkDevice is the
//!     only thing destroyed — host-side resources (the original VkImage,
//!     the timeline semaphore, the registry entry) keep working.
//!
//! Uses `streamlib_adapter_abi::testing::SubprocessCrashHarness` which
//! spawns the helper, waits a configurable delay, SIGKILLs (the helper
//! also self-aborts but SIGKILL is the harness contract), then polls a
//! caller-provided observer until cleanup is confirmed or the timeout
//! fires.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use std::os::fd::IntoRawFd;
use std::os::unix::io::AsRawFd;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use streamlib_adapter_abi::testing::{CrashTiming, SubprocessCrashHarness};

#[test]
fn subprocess_crash_mid_write_does_not_break_host_adapter() {
    let host = match common::HostFixture::try_new() {
        Some(h) => h,
        None => {
            println!("subprocess_crash_mid_write: skipping — no Vulkan");
            return;
        }
    };

    let surface = host.register_surface(44, 64, 64);
    {
        let _w = host
            .ctx
            .acquire_write(&surface.descriptor)
            .expect("warm-up write");
    }

    // Build the helper Command + a socketpair-style fd handoff.
    let (parent_sock, child_sock) =
        std::os::unix::net::UnixStream::pair().expect("socketpair");
    let child_fd = child_sock.into_raw_fd();
    unsafe {
        let flags = libc::fcntl(child_fd, libc::F_GETFD);
        libc::fcntl(child_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
    }
    let bin_path = common::vulkan_adapter_subprocess_helper_path();
    let mut cmd = Command::new(bin_path);
    cmd.arg("crash-mid-write")
        .env("STREAMLIB_HELPER_SOCKET_FD", child_fd.to_string());

    // Send the descriptor over the socketpair before the harness runs
    // — the helper reads it on startup, then crashes.
    let dma_buf_fd = surface
        .texture
        .vulkan_inner()
        .export_dma_buf_fd()
        .expect("export DMA-BUF");
    let sync_fd = Arc::clone(&surface.timeline)
        .export_opaque_fd()
        .expect("export sync_fd");
    let req = common::helper_descriptor("crash-mid-write", &surface, 1, None);

    // Pre-send the request before the harness spawns; libc::sendmsg on
    // a socketpair is non-blocking under default buffer sizes for this
    // payload size.
    common::send_helper_request(&parent_sock, &req, &[dma_buf_fd], sync_fd)
        .expect("send helper request");

    let observed_disconnect = Arc::new(AtomicBool::new(false));
    let parent_fd = parent_sock.as_raw_fd();
    let observed_clone = Arc::clone(&observed_disconnect);

    let outcome = SubprocessCrashHarness::new(cmd)
        .with_timing(CrashTiming::AfterDelay(Duration::from_millis(150)))
        .with_cleanup_timeout(Duration::from_secs(5))
        .with_post_spawn(move |_child| {
            // Close our copy of the inherited fd so the kernel can
            // reduce the refcount cleanly when the child dies.
            unsafe { libc::close(child_fd) };
            Ok(())
        })
        .run(|| {
            // Observe cleanup by reading from parent_sock — when the
            // child is gone, read returns 0 (EOF) immediately.
            let mut buf = [0u8; 1];
            let n = unsafe {
                libc::recv(
                    parent_fd,
                    buf.as_mut_ptr() as *mut libc::c_void,
                    1,
                    libc::MSG_DONTWAIT | libc::MSG_PEEK,
                )
            };
            if n == 0 {
                observed_clone.store(true, Ordering::Release);
                Ok(())
            } else if n < 0 {
                let err = std::io::Error::last_os_error();
                if matches!(err.raw_os_error(), Some(libc::EAGAIN)) {
                    Err("still alive")
                } else {
                    // Connection reset / pipe — child fully gone.
                    observed_clone.store(true, Ordering::Release);
                    Ok(())
                }
            } else {
                Err("unexpected data on socket from crashing child")
            }
        })
        .expect("crash harness must not error");

    assert!(observed_disconnect.load(Ordering::Acquire));
    assert!(
        outcome.cleanup_latency.as_secs() < 5,
        "cleanup observed too late: {:?}",
        outcome.cleanup_latency
    );

    // The host's adapter should still function. The previous timeline
    // value is whatever the host warmed up to (1); a fresh write
    // advances it by one.
    let before = surface.timeline.current_value().unwrap();
    {
        let _w = host
            .ctx
            .acquire_write(&surface.descriptor)
            .expect("post-crash acquire_write");
    }
    let after = surface.timeline.current_value().unwrap();
    assert!(
        after > before,
        "timeline should advance after crash; before={before} after={after}"
    );
}
