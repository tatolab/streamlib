// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_opengl::tests::subprocess_crash_mid_write` —
//! exercises the public `SubprocessCrashHarness` from
//! `streamlib-adapter-abi::testing` to confirm the OpenGL adapter's
//! crash path matches the contract.
//!
//! The harness spawns the helper subprocess
//! (`opengl_adapter_subprocess_helper`), the helper imports a
//! DMA-BUF passed via SCM_RIGHTS into its own EGL+GL stack, then —
//! per the `crash-mid-write` role — `abort()`s before responding.
//! The host-side observation closure watches the inherited pipe FD
//! and reports cleanup once the kernel reaps the subprocess.
//!
//! NOTE: this test is intentionally narrow — it doesn't try to
//! catch every possible host-side resource leak. Wider crash-
//! recovery semantics live in the surface-share watchdog (#522);
//! this test just proves the harness wiring is correct against the
//! adapter's own subprocess shape.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use std::os::fd::{AsRawFd, IntoRawFd};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::Duration;

use streamlib_adapter_abi::testing::{CrashTiming, SubprocessCrashHarness};

use common::HostFixture;

#[test]
fn subprocess_crash_mid_write_observed_by_harness() {
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!("subprocess_crash_mid_write: skipping — no Vulkan or no EGL");
            return;
        }
    };
    let surface = fixture.register_surface(42, 64, 64);
    let dma_buf_fd = surface
        .texture
        .vulkan_inner()
        .export_dma_buf_fd()
        .expect("export DMA-BUF");
    let plane = surface
        .texture
        .vulkan_inner()
        .dma_buf_plane_layout()
        .expect("dma_buf_plane_layout");
    let modifier = surface.texture.vulkan_inner().chosen_drm_format_modifier();

    // Pipe pair — the parent observes EOF on the read end once the
    // subprocess is reaped (kernel closes the inherited write end on
    // exit / SIGKILL). This is the standard "did cleanup fire?"
    // observation primitive.
    let mut pipe_fds = [-1i32; 2];
    unsafe {
        let r = libc::pipe(pipe_fds.as_mut_ptr());
        assert_eq!(r, 0, "pipe() failed: {}", std::io::Error::last_os_error());
        // Make the read end non-blocking so the observe loop
        // doesn't wedge if the child hasn't been reaped yet.
        let flags = libc::fcntl(pipe_fds[0], libc::F_GETFL);
        libc::fcntl(pipe_fds[0], libc::F_SETFL, flags | libc::O_NONBLOCK);
        // Clear FD_CLOEXEC on the write end so it survives execve.
        let wf = libc::fcntl(pipe_fds[1], libc::F_GETFD);
        libc::fcntl(pipe_fds[1], libc::F_SETFD, wf & !libc::FD_CLOEXEC);
    }
    let read_fd = pipe_fds[0];
    let write_fd = pipe_fds[1];

    // Build the helper command. The helper reads the DMA-BUF over a
    // socketpair (SCM_RIGHTS) — set that up here.
    let (parent_sock, child_sock) = UnixStream::pair().expect("socketpair");
    let child_fd = child_sock.into_raw_fd();
    unsafe {
        let f = libc::fcntl(child_fd, libc::F_GETFD);
        libc::fcntl(child_fd, libc::F_SETFD, f & !libc::FD_CLOEXEC);
    }

    let bin_path = env!("CARGO_BIN_EXE_opengl_adapter_subprocess_helper");
    let mut cmd = Command::new(bin_path);
    cmd.arg("crash-mid-write")
        .env("STREAMLIB_HELPER_SOCKET_FD", child_fd.to_string())
        // The write pipe FD is what the parent observes — we don't
        // tell the helper about it; the kernel just gives the
        // subprocess a copy that gets closed on reap.
        .env("STREAMLIB_HELPER_OBSERVE_FD", write_fd.to_string())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    // Send the surface descriptor + DMA-BUF fd over the
    // socketpair as soon as the child is up. The harness's
    // post_spawn hook is the canonical place for this.
    let request = serde_json::json!({
        "width": surface.width,
        "height": surface.height,
        // Match the host's `Bgra8Unorm` allocation — see common.rs.
        "drm_fourcc": streamlib_adapter_opengl::DRM_FORMAT_ARGB8888,
        "drm_format_modifier": modifier,
        "plane_offset": plane[0].0,
        "plane_stride": plane[0].1,
    });
    let request_bytes = serde_json::to_vec(&request).expect("serialize");
    let parent_sock_for_hook = parent_sock;

    let outcome = SubprocessCrashHarness::new(cmd)
        .with_timing(CrashTiming::AfterDelay(Duration::from_millis(200)))
        .with_cleanup_timeout(Duration::from_secs(3))
        .with_post_spawn(move |_child| {
            // Send the request + DMA-BUF FD; close our copy of the
            // child's socket end and the write end of the pipe so
            // the kernel-side cleanup observation fires on SIGKILL.
            streamlib_surface_client::send_message_with_fds(
                &parent_sock_for_hook,
                &request_bytes,
                &[dma_buf_fd],
            )?;
            unsafe {
                libc::close(child_fd);
                libc::close(write_fd);
                libc::close(dma_buf_fd);
            }
            Ok(())
        })
        .run(|| {
            // Watch for EOF on the read end of the pipe — when the
            // kernel reaps the subprocess, our copy of the write
            // end is the only remaining ref, so read() returns 0.
            let mut buf = [0u8; 1];
            let n = unsafe {
                libc::read(read_fd, buf.as_mut_ptr() as *mut _, 1)
            };
            // n == 0 means EOF (cleanup observed); n < 0 with EAGAIN
            // means "still alive, try again"; n > 0 means data
            // (shouldn't happen — helper doesn't write).
            if n == 0 {
                Ok(())
            } else {
                Err("subprocess still has open write end")
            }
        })
        .expect("crash harness ran");

    unsafe {
        libc::close(read_fd);
    }

    // Cleanup latency: the harness reports kill→cleanup latency.
    // For a SIGKILL'd child the kernel reaps quickly; assert it
    // came in well under the timeout.
    assert!(
        outcome.cleanup_latency < Duration::from_secs(3),
        "cleanup_latency {:?} exceeded budget",
        outcome.cleanup_latency
    );
    let exit_status = outcome.exit_status.expect("child waited");
    assert!(
        !exit_status.success(),
        "subprocess SIGKILL'd by harness must NOT report success: {exit_status:?}"
    );
}

// Silence unused-warning for AsRawFd — we use it implicitly via the
// UnixStream types above. Kept as a top-level use for future
// debugging hooks.
#[allow(dead_code)]
fn _force_use_as_raw_fd(s: &UnixStream) -> i32 {
    s.as_raw_fd()
}
