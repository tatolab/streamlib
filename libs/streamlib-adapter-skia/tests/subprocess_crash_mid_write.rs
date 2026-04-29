// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_skia::tests::subprocess_crash_mid_write` —
//! exercises the public `SubprocessCrashHarness` from
//! `streamlib-adapter-abi::testing` against a subprocess that has
//! built a full `SkiaGlContext` on top of the OpenGL adapter, drawn
//! into a Skia canvas, and then `abort()`s before its drop hook can
//! call `flush_and_submit_surface`.
//!
//! The harness spawns the helper subprocess
//! (`skia_adapter_subprocess_helper`), the helper imports a DMA-BUF
//! passed via SCM_RIGHTS into its own EGL+GL stack, brings up Skia,
//! issues a draw on the canvas, then — per the `crash-mid-write`
//! role — `abort()`s with the write guard still live. The host-side
//! observation closure watches the inherited pipe FD and reports
//! cleanup once the kernel reaps the subprocess.
//!
//! Independent of the Python bridge — exercises the Rust adapter
//! directly against `HostVulkanDevice`. The Python wrapper rides the
//! same code paths transitively (`streamlib.adapters.skia.SkiaContext`
//! composes on `OpenGLContext`, just as `SkiaGlContext` does in Rust).
//!
//! Mirror of
//! `streamlib-adapter-opengl/tests/subprocess_crash_mid_write.rs` —
//! the wrappage difference is that the helper here also constructs a
//! `SkiaGlContext` and acquires a Skia write guard before the abort,
//! exercising the additional drop-order chain
//! (`SkiaGlWriteView::drop` → `flush_and_submit_surface` →
//! `lock_make_current` → `OpenGlSurfaceAdapter::end_write_access` →
//! `glFinish`) that the OpenGL adapter test alone does not cover.

#![cfg(target_os = "linux")]

use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::Duration;

use streamlib::core::context::GpuContext;
use streamlib::core::rhi::TextureFormat;
use streamlib_adapter_abi::testing::{CrashTiming, SubprocessCrashHarness};
use streamlib_adapter_opengl::DRM_FORMAT_ARGB8888;

fn try_init_gpu() -> Option<GpuContext> {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter(
            "streamlib_adapter_skia=debug,\
             streamlib_adapter_opengl=warn,\
             streamlib=warn",
        )
        .try_init();
    GpuContext::init_for_platform_sync().ok()
}

#[test]
fn subprocess_crash_mid_skia_write_observed_by_harness() {
    let gpu = match try_init_gpu() {
        Some(g) => g,
        None => {
            println!(
                "subprocess_crash_mid_skia_write: skipping — no Vulkan / no GPU"
            );
            return;
        }
    };

    // Allocate a host render-target DMA-BUF the subprocess will import
    // through EGL + GL + Skia.
    let texture = gpu
        .acquire_render_target_dma_buf_image(64, 64, TextureFormat::Bgra8Unorm)
        .expect("acquire_render_target_dma_buf_image");
    let dma_buf_fd = texture
        .vulkan_inner()
        .export_dma_buf_fd()
        .expect("export DMA-BUF");
    let plane = texture
        .vulkan_inner()
        .dma_buf_plane_layout()
        .expect("dma_buf_plane_layout");
    let modifier = texture.vulkan_inner().chosen_drm_format_modifier();

    // Pipe pair — the parent observes EOF on the read end once the
    // subprocess is reaped (kernel closes the inherited write end on
    // exit / SIGKILL). Standard "did cleanup fire?" observation
    // primitive, mirror of the OpenGL adapter's crash test.
    let mut pipe_fds = [-1i32; 2];
    unsafe {
        let r = libc::pipe(pipe_fds.as_mut_ptr());
        assert_eq!(r, 0, "pipe() failed: {}", std::io::Error::last_os_error());
        let flags = libc::fcntl(pipe_fds[0], libc::F_GETFL);
        libc::fcntl(pipe_fds[0], libc::F_SETFL, flags | libc::O_NONBLOCK);
        let wf = libc::fcntl(pipe_fds[1], libc::F_GETFD);
        libc::fcntl(pipe_fds[1], libc::F_SETFD, wf & !libc::FD_CLOEXEC);
    }
    let read_fd = pipe_fds[0];
    let write_fd = pipe_fds[1];

    // The helper reads the surface descriptor + DMA-BUF fd over a
    // socketpair (SCM_RIGHTS) — set that up here.
    let (parent_sock, child_sock) = UnixStream::pair().expect("socketpair");
    let child_fd = child_sock.into_raw_fd();
    unsafe {
        let f = libc::fcntl(child_fd, libc::F_GETFD);
        libc::fcntl(child_fd, libc::F_SETFD, f & !libc::FD_CLOEXEC);
    }

    let bin_path = env!("CARGO_BIN_EXE_skia_adapter_subprocess_helper");
    let mut cmd = Command::new(bin_path);
    cmd.arg("crash-mid-write")
        .env("STREAMLIB_HELPER_SOCKET_FD", child_fd.to_string())
        .env("STREAMLIB_HELPER_OBSERVE_FD", write_fd.to_string())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let request = serde_json::json!({
        "width": 64u32,
        "height": 64u32,
        // Vulkan `Bgra8Unorm` is "memory: B,G,R,A". DRM_FORMAT_ARGB8888
        // is the matching fourcc on a little-endian host — see
        // `streamlib-adapter-opengl/tests/common.rs` for the full
        // reasoning. Using ABGR8888 here would silently swap R↔B on
        // every GL-side write because the EGL importer trusts the
        // declared fourcc.
        "drm_fourcc": DRM_FORMAT_ARGB8888,
        "drm_format_modifier": modifier,
        "plane_offset": plane[0].0,
        "plane_stride": plane[0].1,
    });
    let request_bytes = serde_json::to_vec(&request).expect("serialize");
    let parent_sock_for_hook = parent_sock;

    // Bump the cleanup timeout vs. the OpenGL adapter's 3s budget.
    // The Skia helper does noticeably more work in `crash-mid-write`
    // (constructs `SkiaGlContext` via `MakeGL`, acquires a write
    // guard, issues a Skia draw) before reaching the `abort()`. On a
    // cold cache that startup can run several hundred ms — give the
    // harness room without regressing on signal.
    let outcome = SubprocessCrashHarness::new(cmd)
        .with_timing(CrashTiming::AfterDelay(Duration::from_millis(400)))
        .with_cleanup_timeout(Duration::from_secs(8))
        .with_post_spawn(move |_child| {
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
            let mut buf = [0u8; 1];
            let n = unsafe { libc::read(read_fd, buf.as_mut_ptr() as *mut _, 1) };
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

    assert!(
        outcome.cleanup_latency < Duration::from_secs(8),
        "cleanup_latency {:?} exceeded budget",
        outcome.cleanup_latency
    );
    let exit_status = outcome.exit_status.expect("child waited");
    assert!(
        !exit_status.success(),
        "subprocess SIGKILL'd by harness must NOT report success: {exit_status:?}"
    );

    // Keep the host texture live until after the harness asserts the
    // child exited — the registered DMA-BUF FD has to remain valid
    // while the subprocess is importing it.
    drop(texture);
    drop(gpu);
}
