// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A subprocess that holds a live Skia write guard on an imported DMA-BUF
//! `abort()`s; the host observes the kernel reaping it even though the drop
//! chain (`SkiaGlWriteView::drop` → `flush_and_submit_surface` →
//! `lock_make_current` → `OpenGlSurfaceAdapter::end_write_access` →
//! `glFinish`) never runs.

#![cfg(target_os = "linux")]

use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};
use std::time::Duration;
use streamlib::sdk::engine::HostTextureExt;

use streamlib::sdk::context::GpuContext;
use streamlib::sdk::rhi::TextureFormat;
use streamlib_adapter_opengl::DRM_FORMAT_ARGB8888;
use streamlib_surface_adapter::testing::{CrashTiming, SubprocessCrashHarness};

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
            println!("subprocess_crash_mid_skia_write: skipping — no Vulkan / no GPU");
            return;
        }
    };

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

    // The parent observes EOF on the read end once the kernel closes the
    // child's inherited write end on exit / SIGKILL.
    let mut pipe_fds = [-1i32; 2];
    unsafe {
        let r = libc::pipe2(pipe_fds.as_mut_ptr(), libc::O_CLOEXEC);
        assert_eq!(r, 0, "pipe2() failed: {}", std::io::Error::last_os_error());
        let flags = libc::fcntl(pipe_fds[0], libc::F_GETFL);
        libc::fcntl(pipe_fds[0], libc::F_SETFL, flags | libc::O_NONBLOCK);
        let wf = libc::fcntl(pipe_fds[1], libc::F_GETFD);
        libc::fcntl(pipe_fds[1], libc::F_SETFD, wf & !libc::FD_CLOEXEC);
    }
    let read_fd = pipe_fds[0];
    let write_fd = pipe_fds[1];

    // The helper reads the surface descriptor + DMA-BUF fd over SCM_RIGHTS.
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
        // Vulkan `Bgra8Unorm` is memory B,G,R,A, which is DRM_FORMAT_ARGB8888
        // on a little-endian host; the EGL importer trusts the declared
        // fourcc, so ABGR8888 would swap R↔B on every GL-side write.
        "drm_fourcc": DRM_FORMAT_ARGB8888,
        "drm_format_modifier": modifier,
        "plane_offset": plane[0].0,
        "plane_stride": plane[0].1,
    });
    let request_bytes = serde_json::to_vec(&request).expect("serialize");
    let parent_sock_for_hook = parent_sock;

    // Skia's `MakeGL` bring-up before the `abort()` can take several
    // hundred ms on a cold cache.
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
    // SIGABRT (the helper's own abort) or SIGKILL (the harness watchdog)
    // proves the crash path was reached; a setup failure exits with a code.
    let exit_status = outcome.exit_status.expect("child waited");
    assert!(
        exit_status.signal().is_some(),
        "expected signal-terminated subprocess (SIGABRT from helper's \
         own abort() or SIGKILL from harness watchdog); got {exit_status:?} \
         which means setup failed before the crash path was reached"
    );

    // The DMA-BUF must stay valid while the subprocess imports it.
    drop(texture);
    drop(gpu);
}
