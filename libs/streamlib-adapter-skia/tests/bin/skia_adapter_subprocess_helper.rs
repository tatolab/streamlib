// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Subprocess test helper for the Skia adapter's crash-mid-write test.
//!
//! Receives a DMA-BUF FD over `STREAMLIB_HELPER_SOCKET_FD`, imports it
//! into the GL-backed Skia adapter (`SkiaGlContext` on top of
//! `OpenGlSurfaceAdapter`), drives a Skia draw, then either signals
//! success or `abort()`s mid-flight depending on argv[1].
//!
//! Roles:
//! - `wait-only` — import + register + signal ready + park until
//!   killed.
//! - `crash-mid-write` — import + register + acquire a Skia write
//!   guard + draw a known shape into the canvas + `abort()` BEFORE the
//!   guard's drop hook can call `flush_and_submit_surface` and the
//!   inner OpenGL release's `glFinish`. Validates that the host's
//!   per-surface state survives a SIGKILL on a subprocess that holds
//!   live Skia + EGL state.

#![cfg(target_os = "linux")]

use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::sync::Arc;

use streamlib_adapter_abi::{
    StreamlibSurface, SurfaceFormat, SurfaceSyncState, SurfaceTransportHandle, SurfaceUsage,
};
use streamlib_adapter_opengl::{
    EglRuntime, HostSurfaceRegistration, OpenGlContext, OpenGlSurfaceAdapter,
};
use streamlib_adapter_skia::SkiaGlContext;

const HELPER_SURFACE_ID: u64 = 0xfeed_face;

#[derive(Debug, serde::Deserialize)]
struct HelperRequest {
    width: u32,
    height: u32,
    drm_fourcc: u32,
    drm_format_modifier: u64,
    plane_offset: u64,
    plane_stride: u64,
}

#[derive(Debug, serde::Serialize)]
struct HelperResponse {
    ok: bool,
    note: String,
}

fn die(socket: Option<&UnixStream>, msg: String) -> ExitCode {
    eprintln!("[skia-helper] FATAL: {msg}");
    if let Some(s) = socket {
        let resp = HelperResponse {
            ok: false,
            note: msg,
        };
        let body = serde_json::to_vec(&resp).unwrap_or_default();
        let _ = streamlib_surface_client::send_message_with_fds(s, &body, &[]);
    }
    ExitCode::from(1)
}

fn run() -> ExitCode {
    let role = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "wait-only".to_string());
    let sock_fd_str = match std::env::var("STREAMLIB_HELPER_SOCKET_FD") {
        Ok(v) => v,
        Err(_) => return die(None, "STREAMLIB_HELPER_SOCKET_FD unset".into()),
    };
    let sock_fd: RawFd = match sock_fd_str.parse() {
        Ok(v) => v,
        Err(_) => return die(None, "STREAMLIB_HELPER_SOCKET_FD not an integer".into()),
    };
    let socket = unsafe { UnixStream::from_raw_fd(sock_fd) };

    // Read length-prefixed JSON + DMA-BUF fd via SCM_RIGHTS.
    let mut len_buf = [0u8; 4];
    let mut total = 0;
    while total < 4 {
        let n = unsafe {
            libc::read(
                socket.as_raw_fd(),
                len_buf[total..].as_mut_ptr() as *mut libc::c_void,
                4 - total,
            )
        };
        if n <= 0 {
            return die(Some(&socket), "read length prefix failed".into());
        }
        total += n as usize;
    }
    let msg_len = u32::from_be_bytes(len_buf) as usize;
    let (payload, fds) = match streamlib_surface_client::recv_message_with_fds(
        &socket, msg_len, 1,
    ) {
        Ok(p) => p,
        Err(e) => return die(Some(&socket), format!("recv_message_with_fds: {e}")),
    };
    let req: HelperRequest = match serde_json::from_slice(&payload) {
        Ok(r) => r,
        Err(e) => return die(Some(&socket), format!("parse request: {e}")),
    };
    let dma_buf_fd = match fds.first() {
        Some(&fd) => fd,
        None => return die(Some(&socket), "no DMA-BUF fd received".into()),
    };

    // Bring up the OpenGL adapter (EGL display + GL context + the
    // adapter's `OpenGlSurfaceAdapter`), import the host's DMA-BUF, and
    // wrap with a `SkiaGlContext`. Mirrors the customer-side path the
    // Python wrapper exercises, just in Rust.
    let runtime = match EglRuntime::new() {
        Ok(r) => r,
        Err(e) => return die(Some(&socket), format!("EglRuntime::new: {e}")),
    };
    let adapter = Arc::new(OpenGlSurfaceAdapter::new(runtime));
    let registration = HostSurfaceRegistration {
        dma_buf_fd,
        width: req.width,
        height: req.height,
        drm_fourcc: req.drm_fourcc,
        drm_format_modifier: req.drm_format_modifier,
        plane_offset: req.plane_offset,
        plane_stride: req.plane_stride,
    };
    if let Err(e) = adapter.register_host_surface(HELPER_SURFACE_ID, registration) {
        return die(Some(&socket), format!("register_host_surface: {e}"));
    }
    // EGL dups the FD on import; close our copy.
    unsafe { libc::close(dma_buf_fd) };

    let opengl_ctx = OpenGlContext::new(Arc::clone(&adapter));
    let skia_ctx = match SkiaGlContext::new(&opengl_ctx) {
        Ok(c) => c,
        Err(e) => return die(Some(&socket), format!("SkiaGlContext::new: {e}")),
    };

    // Tell the parent we're up. The parent's harness only starts its
    // SIGKILL countdown after it has confirmed the helper reached this
    // point.
    let resp = HelperResponse {
        ok: true,
        note: format!("registered + skia_gl_context built ({role})"),
    };
    let body = serde_json::to_vec(&resp).unwrap_or_default();
    let _ = streamlib_surface_client::send_message_with_fds(&socket, &body, &[]);

    match role.as_str() {
        "wait-only" => {
            // Park forever — the parent SIGKILLs us via the harness.
            std::thread::park();
            ExitCode::SUCCESS
        }
        "crash-mid-write" => crash_mid_skia_write(&skia_ctx, req.width, req.height),
        other => die(Some(&socket), format!("unknown role {other}")),
    }
}

/// Acquire a Skia write guard, issue a draw command stream, and
/// `abort()` BEFORE the guard's drop hook fires. Drop is what would
/// normally call `flush_and_submit_surface` (drain Skia's GPU work)
/// and `glFinish` (drain GL via the OpenGL adapter's release). By
/// `abort()`ing before drop, we're "mid-`flush_and_submit_surface`"
/// in the relevant sense — the host has work in flight that the
/// subprocess never got around to draining, and the host's per-surface
/// state must still survive the SIGKILL that follows.
fn crash_mid_skia_write(skia_ctx: &SkiaGlContext, width: u32, height: u32) -> ExitCode {
    let surface_descriptor = StreamlibSurface::new(
        HELPER_SURFACE_ID,
        width,
        height,
        SurfaceFormat::Bgra8,
        SurfaceUsage::RENDER_TARGET | SurfaceUsage::SAMPLED,
        SurfaceTransportHandle::empty(),
        SurfaceSyncState::default(),
    );

    let mut guard = match skia_ctx.acquire_write(&surface_descriptor) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("[skia-helper] acquire_write: {e}");
            std::process::abort();
        }
    };
    {
        let view = guard.view_mut();
        let sk_surface = view.surface_mut();
        let canvas = sk_surface.canvas();
        canvas.clear(skia_safe::Color::BLUE);
        let mut paint = skia_safe::Paint::default();
        paint.set_color(skia_safe::Color::RED);
        paint.set_anti_alias(true);
        let cx = (width as f32) * 0.5;
        let cy = (height as f32) * 0.5;
        let radius = (width.min(height) as f32) * 0.35;
        canvas.draw_circle((cx, cy), radius, &paint);
    }
    // Spec'd to crash with the guard still live — `abort()` skips
    // `Drop`, so neither `flush_and_submit_surface` nor `glFinish`
    // runs.
    std::process::abort();
}

fn main() -> ExitCode {
    run()
}
