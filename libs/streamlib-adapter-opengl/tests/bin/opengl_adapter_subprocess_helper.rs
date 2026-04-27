// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Subprocess test helper for the OpenGL adapter's crash-mid-write
//! test. Receives a DMA-BUF FD over `STREAMLIB_HELPER_SOCKET_FD`,
//! imports it into its own EGL+GL stack via the same path the
//! adapter uses, optionally renders into the texture, then either
//! signals success or `abort()`s mid-flight depending on argv[1].
//!
//! Roles:
//! - `wait-only` — import + signal "ready" + sleep until killed.
//! - `crash-mid-write` — import + bind FBO + draw + `abort()` before
//!   the parent sees a response.

#![cfg(target_os = "linux")]

use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::sync::Arc;

use streamlib_adapter_opengl::{
    EglRuntime, HostSurfaceRegistration, OpenGlSurfaceAdapter,
};

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
    eprintln!("[opengl-helper] FATAL: {msg}");
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
    if let Err(e) = adapter.register_host_surface(0xfeed_face, registration) {
        return die(Some(&socket), format!("register_host_surface: {e}"));
    }
    // EGL dups the FD on import; close our copy.
    unsafe { libc::close(dma_buf_fd) };

    let resp = HelperResponse {
        ok: true,
        note: format!("registered {role}"),
    };
    let body = serde_json::to_vec(&resp).unwrap_or_default();
    let _ = streamlib_surface_client::send_message_with_fds(&socket, &body, &[]);

    match role.as_str() {
        "wait-only" => {
            // Park forever — the parent SIGKILLs us via the harness.
            std::thread::park();
            ExitCode::SUCCESS
        }
        "crash-mid-write" => {
            // Spec'd to crash before the parent has observed cleanup.
            std::process::abort();
        }
        other => die(Some(&socket), format!("unknown role {other}")),
    }
}

fn main() -> ExitCode {
    run()
}
