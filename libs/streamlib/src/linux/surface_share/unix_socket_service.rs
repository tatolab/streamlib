// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-runtime Unix socket surface-sharing service.
//!
//! Each `StreamRuntime` owns one of these listening on a unique socket under
//! `$XDG_RUNTIME_DIR`. Polyglot subprocesses connect via `connect_to_surface_share_socket`
//! / `send_request_with_fds` (from [`streamlib_surface_client`]) and exchange
//! DMA-BUF fds over `SCM_RIGHTS`. Surfaces may carry up to
//! [`streamlib_surface_client::MAX_DMA_BUF_PLANES`] plane fds — one per plane
//! for multi-plane DMA-BUFs (e.g. NV12 with separate Y and UV allocations).

use std::io::Read;
use std::os::unix::io::RawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use streamlib_surface_client::{
    recv_message_with_fds, send_message_with_fds, MAX_DMA_BUF_PLANES,
};

use super::state::{SurfaceShareState, SurfaceRegistration};

pub struct UnixSocketSurfaceService {
    state: SurfaceShareState,
    socket_path: PathBuf,
    listener_thread: Option<thread::JoinHandle<()>>,
    shutdown_flag: Arc<std::sync::atomic::AtomicBool>,
}

impl UnixSocketSurfaceService {
    pub fn new(state: SurfaceShareState, socket_path: PathBuf) -> Self {
        Self {
            state,
            socket_path,
            listener_thread: None,
            shutdown_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    pub fn start(&mut self) -> Result<(), String> {
        if self.socket_path.exists() {
            std::fs::remove_file(&self.socket_path)
                .map_err(|e| format!("Failed to remove stale socket: {}", e))?;
        }

        if let Some(parent) = self.socket_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create socket directory: {}", e))?;
        }

        let listener = UnixListener::bind(&self.socket_path)
            .map_err(|e| format!("Failed to bind Unix socket at {:?}: {}", self.socket_path, e))?;

        listener
            .set_nonblocking(true)
            .map_err(|e| format!("Failed to set non-blocking: {}", e))?;

        let state = self.state.clone();
        let shutdown_flag = self.shutdown_flag.clone();

        let handle = thread::spawn(move || {
            run_listener(listener, state, shutdown_flag);
        });

        self.listener_thread = Some(handle);

        tracing::info!(
            "[Surface share] Unix socket surface service listening on {:?}",
            self.socket_path
        );

        Ok(())
    }

    pub fn stop(&mut self) {
        self.shutdown_flag
            .store(true, std::sync::atomic::Ordering::Relaxed);

        if let Some(handle) = self.listener_thread.take() {
            let _ = handle.join();
        }

        if self.socket_path.exists() {
            let _ = std::fs::remove_file(&self.socket_path);
        }

        tracing::info!("[Surface share] Unix socket surface service stopped");
    }
}

impl Drop for UnixSocketSurfaceService {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_listener(
    listener: UnixListener,
    state: SurfaceShareState,
    shutdown_flag: Arc<std::sync::atomic::AtomicBool>,
) {
    loop {
        if shutdown_flag.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }

        match listener.accept() {
            Ok((stream, _addr)) => {
                let state = state.clone();
                thread::spawn(move || {
                    if let Err(e) = handle_client_connection(stream, state) {
                        tracing::debug!("[Surface share] Client connection ended: {}", e);
                    }
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => {
                if shutdown_flag.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                tracing::warn!("[Surface share] Unix socket accept error: {}", e);
                thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
}

fn handle_client_connection(
    mut stream: UnixStream,
    state: SurfaceShareState,
) -> Result<(), std::io::Error> {
    stream.set_nonblocking(false)?;

    loop {
        let mut len_buf = [0u8; 4];
        match stream.read_exact(&mut len_buf) {
            Ok(()) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(());
            }
            Err(e) => return Err(e),
        }

        let msg_len = u32::from_be_bytes(len_buf) as usize;
        if msg_len > 1024 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Message too large",
            ));
        }

        let (json_bytes, received_fds) =
            recv_message_with_fds(&stream, msg_len, MAX_DMA_BUF_PLANES)?;

        let request: serde_json::Value = serde_json::from_slice(&json_bytes).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("Invalid JSON: {}", e))
        })?;

        let op = request.get("op").and_then(|v| v.as_str()).unwrap_or("");

        let (response, reply_fds) = match op {
            "register" => handle_register(&state, &request, &received_fds),
            "lookup" | "check_out" => handle_lookup(&state, &request),
            "unregister" | "release" => handle_unregister(&state, &request),
            "check_in" => handle_check_in(&state, &request, &received_fds),
            _ => (
                serde_json::json!({"error": format!("unknown operation: {}", op)}),
                Vec::new(),
            ),
        };

        // Close every fd the peer sent us — handlers that wanted to keep
        // ownership `dup`'d during registration.
        for fd in &received_fds {
            unsafe { libc::close(*fd) };
        }

        let response_bytes = serde_json::to_vec(&response).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to serialize response: {}", e),
            )
        })?;

        send_message_with_fds(&stream, &response_bytes, &reply_fds)?;

        for fd in &reply_fds {
            unsafe { libc::close(*fd) };
        }
    }
}

/// Extract `plane_sizes` and `plane_offsets` from a JSON request body.
/// Returns `(plane_sizes, plane_offsets)` where each vec's length matches
/// `expected_plane_count`, falling back to `[0]` and `[0]` when the peer
/// sent no arrays (single-plane back-compat) or when a provided array has a
/// different length than the fd set.
///
/// Returning `None` is an explicit wire-protocol violation (mismatched
/// arrays, negative values); the handler should error out instead of
/// guessing.
fn parse_plane_arrays(
    request: &serde_json::Value,
    expected_plane_count: usize,
) -> Option<(Vec<u64>, Vec<u64>)> {
    let parse_arr = |key: &str| -> Option<Option<Vec<u64>>> {
        match request.get(key) {
            None => Some(None),
            Some(v) => v
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|el| el.as_u64())
                        .collect::<Option<Vec<u64>>>()
                })
                .map(Some)
                .unwrap_or(Some(None)),
        }
    };

    let sizes_opt = parse_arr("plane_sizes")?;
    let offsets_opt = parse_arr("plane_offsets")?;

    let sizes = match sizes_opt {
        Some(v) if v.len() == expected_plane_count => v,
        Some(_) => return None,
        None if expected_plane_count <= 1 => vec![0u64; expected_plane_count.max(1)],
        None => return None,
    };
    let offsets = match offsets_opt {
        Some(v) if v.len() == expected_plane_count => v,
        Some(_) => return None,
        None if expected_plane_count <= 1 => vec![0u64; expected_plane_count.max(1)],
        None => return None,
    };
    Some((sizes, offsets))
}

fn handle_register(
    state: &SurfaceShareState,
    request: &serde_json::Value,
    received_fds: &[RawFd],
) -> (serde_json::Value, Vec<RawFd>) {
    let surface_id = match request.get("surface_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return (serde_json::json!({"error": "missing surface_id"}), Vec::new()),
    };

    let runtime_id = request
        .get("runtime_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    if received_fds.is_empty() {
        return (
            serde_json::json!({"error": "missing DMA-BUF fd(s)"}),
            Vec::new(),
        );
    }
    if received_fds.len() > MAX_DMA_BUF_PLANES {
        return (
            serde_json::json!({
                "error": format!(
                    "too many plane fds: {} > MAX_DMA_BUF_PLANES ({})",
                    received_fds.len(), MAX_DMA_BUF_PLANES
                )
            }),
            Vec::new(),
        );
    }

    let (plane_sizes, plane_offsets) = match parse_plane_arrays(request, received_fds.len()) {
        Some(arrays) => arrays,
        None => {
            return (
                serde_json::json!({"error": "plane_sizes/plane_offsets length mismatch"}),
                Vec::new(),
            )
        }
    };

    let width = request.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let height = request.get("height").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let format = request.get("format").and_then(|v| v.as_str()).unwrap_or("unknown");
    let resource_type = request
        .get("resource_type")
        .and_then(|v| v.as_str())
        .unwrap_or("pixel_buffer");

    let mut dup_fds: Vec<RawFd> = Vec::with_capacity(received_fds.len());
    for fd in received_fds {
        let dup_fd = unsafe { libc::dup(*fd) };
        if dup_fd < 0 {
            for d in &dup_fds {
                unsafe { libc::close(*d) };
            }
            return (
                serde_json::json!({"error": "failed to dup DMA-BUF fd"}),
                Vec::new(),
            );
        }
        dup_fds.push(dup_fd);
    }

    match state.register_surface(SurfaceRegistration {
        surface_id,
        runtime_id,
        dma_buf_fds: dup_fds,
        plane_sizes,
        plane_offsets,
        width,
        height,
        format,
        resource_type,
    }) {
        Ok(()) => {
            tracing::debug!(
                "[Surface share] register: surface '{}' for runtime '{}' ({} plane(s))",
                surface_id,
                runtime_id,
                received_fds.len(),
            );
            (serde_json::json!({"success": true}), Vec::new())
        }
        Err(leftover) => {
            for fd in &leftover {
                unsafe { libc::close(*fd) };
            }
            tracing::warn!(
                "[Surface share] register: surface '{}' already exists",
                surface_id
            );
            (serde_json::json!({"success": false}), Vec::new())
        }
    }
}

fn handle_lookup(
    state: &SurfaceShareState,
    request: &serde_json::Value,
) -> (serde_json::Value, Vec<RawFd>) {
    let surface_id = match request.get("surface_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return (serde_json::json!({"error": "missing surface_id"}), Vec::new()),
    };

    let (plane_fds, plane_sizes, plane_offsets) = match state.get_surface_planes(surface_id) {
        Some(planes) => planes,
        None => return (serde_json::json!({"error": "surface not found"}), Vec::new()),
    };

    // Dup each stored fd so the kernel-delivered fds in the peer's table are
    // independent of the table's own copies. On partial failure, close every
    // dup we already took.
    let mut dup_fds: Vec<RawFd> = Vec::with_capacity(plane_fds.len());
    for fd in &plane_fds {
        let dup = unsafe { libc::dup(*fd) };
        if dup < 0 {
            for d in &dup_fds {
                unsafe { libc::close(*d) };
            }
            return (
                serde_json::json!({"error": "failed to dup DMA-BUF fd"}),
                Vec::new(),
            );
        }
        dup_fds.push(dup);
    }

    let surfaces = state.get_surfaces();
    let metadata = surfaces.iter().find(|s| s.surface_id == surface_id);
    let (width, height, format, resource_type) = match metadata {
        Some(m) => (m.width, m.height, m.format.as_str(), m.resource_type.as_str()),
        None => (0, 0, "unknown", "pixel_buffer"),
    };
    (
        serde_json::json!({
            "surface_id": surface_id,
            "width": width,
            "height": height,
            "format": format,
            "resource_type": resource_type,
            "plane_sizes": plane_sizes,
            "plane_offsets": plane_offsets,
        }),
        dup_fds,
    )
}

fn handle_unregister(
    state: &SurfaceShareState,
    request: &serde_json::Value,
) -> (serde_json::Value, Vec<RawFd>) {
    let surface_id = match request.get("surface_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return (serde_json::json!({"error": "missing surface_id"}), Vec::new()),
    };

    let runtime_id = request
        .get("runtime_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let released = state.release_surface(surface_id, runtime_id);
    (serde_json::json!({"success": released}), Vec::new())
}

fn handle_check_in(
    state: &SurfaceShareState,
    request: &serde_json::Value,
    received_fds: &[RawFd],
) -> (serde_json::Value, Vec<RawFd>) {
    let runtime_id = request
        .get("runtime_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    if received_fds.is_empty() {
        return (
            serde_json::json!({"error": "missing DMA-BUF fd(s)"}),
            Vec::new(),
        );
    }
    if received_fds.len() > MAX_DMA_BUF_PLANES {
        return (
            serde_json::json!({
                "error": format!(
                    "too many plane fds: {} > MAX_DMA_BUF_PLANES ({})",
                    received_fds.len(), MAX_DMA_BUF_PLANES
                )
            }),
            Vec::new(),
        );
    }

    let (plane_sizes, plane_offsets) = match parse_plane_arrays(request, received_fds.len()) {
        Some(arrays) => arrays,
        None => {
            return (
                serde_json::json!({"error": "plane_sizes/plane_offsets length mismatch"}),
                Vec::new(),
            )
        }
    };

    let width = request.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let height = request.get("height").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let format = request.get("format").and_then(|v| v.as_str()).unwrap_or("unknown");
    let resource_type = request
        .get("resource_type")
        .and_then(|v| v.as_str())
        .unwrap_or("pixel_buffer");

    let surface_id = uuid::Uuid::new_v4().to_string();

    let mut dup_fds: Vec<RawFd> = Vec::with_capacity(received_fds.len());
    for fd in received_fds {
        let dup = unsafe { libc::dup(*fd) };
        if dup < 0 {
            for d in &dup_fds {
                unsafe { libc::close(*d) };
            }
            return (
                serde_json::json!({"error": "failed to dup DMA-BUF fd"}),
                Vec::new(),
            );
        }
        dup_fds.push(dup);
    }

    if let Err(leftover) = state.register_surface(SurfaceRegistration {
        surface_id: &surface_id,
        runtime_id,
        dma_buf_fds: dup_fds,
        plane_sizes,
        plane_offsets,
        width,
        height,
        format,
        resource_type,
    }) {
        for fd in &leftover {
            unsafe { libc::close(*fd) };
        }
    }

    (serde_json::json!({"surface_id": surface_id}), Vec::new())
}

unsafe impl Send for UnixSocketSurfaceService {}
unsafe impl Sync for UnixSocketSurfaceService {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::FromRawFd;
    use streamlib_surface_client::{connect_to_surface_share_socket, send_request_with_fds};

    fn make_memfd_with(contents: &[u8]) -> RawFd {
        use std::io::{Seek, SeekFrom, Write};

        let name = std::ffi::CString::new("streamlib-runtime-surface-share-test").unwrap();
        let fd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
        assert!(fd >= 0, "memfd_create failed: {}", std::io::Error::last_os_error());
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.write_all(contents).expect("memfd write");
        file.seek(SeekFrom::Start(0)).expect("memfd rewind");
        use std::os::unix::io::IntoRawFd;
        file.into_raw_fd()
    }

    fn read_all_from_fd(fd: RawFd) -> Vec<u8> {
        use std::io::{Read, Seek, SeekFrom};

        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.seek(SeekFrom::Start(0)).expect("recv memfd rewind");
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).expect("recv memfd read");
        buf
    }

    fn tmp_socket_path() -> PathBuf {
        let mut p = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!(
            "streamlib-runtime-surface-share-test-{}-{}.sock",
            std::process::id(),
            nanos
        ));
        p
    }

    #[test]
    fn check_in_check_out_roundtrip_preserves_fd_content() {
        let state = SurfaceShareState::new();
        let socket_path = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
        service.start().expect("service start");

        std::thread::sleep(std::time::Duration::from_millis(50));

        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

        let pattern = b"streamlib-runtime-surface-share-test-fd-contents-0123456789";
        let send_fd = make_memfd_with(pattern);

        let check_in_req = serde_json::json!({
            "op": "check_in",
            "runtime_id": "test-runtime",
            "width": 640,
            "height": 480,
            "format": "Bgra32",
            "resource_type": "pixel_buffer",
        });
        let (check_in_resp, check_in_fds) =
            send_request_with_fds(&stream, &check_in_req, &[send_fd], 0).expect("check_in request");
        unsafe { libc::close(send_fd) };
        assert!(check_in_fds.is_empty(), "check_in must not return an fd");
        let surface_id = check_in_resp
            .get("surface_id")
            .and_then(|v| v.as_str())
            .expect("surface_id in response")
            .to_string();
        assert!(!surface_id.is_empty());

        let check_out_req = serde_json::json!({
            "op": "check_out",
            "surface_id": surface_id,
        });
        let (check_out_resp, check_out_fds) =
            send_request_with_fds(&stream, &check_out_req, &[], MAX_DMA_BUF_PLANES)
                .expect("check_out request");
        assert_eq!(check_out_resp.get("width").and_then(|v| v.as_u64()), Some(640));
        assert_eq!(check_out_resp.get("height").and_then(|v| v.as_u64()), Some(480));
        assert_eq!(check_out_resp.get("format").and_then(|v| v.as_str()), Some("Bgra32"));
        assert_eq!(check_out_fds.len(), 1, "single-plane: exactly one fd");
        let received_fd = check_out_fds[0];
        assert!(received_fd >= 0);
        let received = read_all_from_fd(received_fd);
        assert_eq!(received, pattern);

        let release_req = serde_json::json!({
            "op": "release",
            "surface_id": surface_id,
            "runtime_id": "test-runtime",
        });
        let _ = send_request_with_fds(&stream, &release_req, &[], 0).expect("release request");

        drop(stream);
        service.stop();
    }

    #[test]
    fn check_out_unknown_surface_id_returns_error_no_fd() {
        let state = SurfaceShareState::new();
        let socket_path = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
        service.start().expect("service start");
        std::thread::sleep(std::time::Duration::from_millis(50));

        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");
        let req = serde_json::json!({
            "op": "check_out",
            "surface_id": "never-registered",
        });
        let (resp, fds) = send_request_with_fds(&stream, &req, &[], MAX_DMA_BUF_PLANES)
            .expect("check_out request");
        assert!(fds.is_empty());
        assert!(resp.get("error").and_then(|v| v.as_str()).is_some());

        drop(stream);
        service.stop();
    }

    /// Two memfds with distinct content registered under one surface_id via
    /// `check_in` must round-trip intact through `check_out`, including the
    /// plane-layout metadata that lets the consumer mmap each plane at its
    /// own size. This is the defining multi-plane DMA-BUF test.
    #[test]
    fn check_in_check_out_multi_fd_roundtrip() {
        let state = SurfaceShareState::new();
        let socket_path = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
        service.start().expect("service start");
        std::thread::sleep(std::time::Duration::from_millis(50));

        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

        let pattern_y = b"plane-Y-bytes-A123456789";
        let pattern_uv = b"plane-UV-bytes-Z987654321";
        let fd_y = make_memfd_with(pattern_y);
        let fd_uv = make_memfd_with(pattern_uv);

        let check_in_req = serde_json::json!({
            "op": "check_in",
            "runtime_id": "test-runtime",
            "width": 1920,
            "height": 1080,
            "format": "Nv12VideoRange",
            "resource_type": "pixel_buffer",
            "plane_sizes": [pattern_y.len() as u64, pattern_uv.len() as u64],
            "plane_offsets": [0u64, 0u64],
        });
        let (check_in_resp, check_in_fds) =
            send_request_with_fds(&stream, &check_in_req, &[fd_y, fd_uv], 0)
                .expect("check_in request");
        unsafe {
            libc::close(fd_y);
            libc::close(fd_uv);
        }
        assert!(check_in_fds.is_empty(), "check_in must not return any fd");
        let surface_id = check_in_resp
            .get("surface_id")
            .and_then(|v| v.as_str())
            .expect("surface_id in response")
            .to_string();

        let check_out_req = serde_json::json!({
            "op": "check_out",
            "surface_id": surface_id,
        });
        let (check_out_resp, check_out_fds) =
            send_request_with_fds(&stream, &check_out_req, &[], MAX_DMA_BUF_PLANES)
                .expect("check_out request");
        assert_eq!(
            check_out_fds.len(),
            2,
            "both planes delivered via SCM_RIGHTS"
        );
        assert_eq!(
            read_all_from_fd(check_out_fds[0]),
            pattern_y,
            "plane 0 content preserved"
        );
        assert_eq!(
            read_all_from_fd(check_out_fds[1]),
            pattern_uv,
            "plane 1 content preserved"
        );
        let sizes = check_out_resp
            .get("plane_sizes")
            .and_then(|v| v.as_array())
            .expect("plane_sizes array in response");
        assert_eq!(
            sizes
                .iter()
                .map(|v| v.as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![pattern_y.len() as u64, pattern_uv.len() as u64],
        );

        let _ = send_request_with_fds(
            &stream,
            &serde_json::json!({
                "op": "release",
                "surface_id": surface_id,
                "runtime_id": "test-runtime",
            }),
            &[],
            0,
        );

        drop(stream);
        service.stop();
    }

    /// The service must refuse a check_in whose fd count exceeds the plane
    /// cap instead of truncating silently. The check runs in the wire helper
    /// before the handler, so this exercises the shared back-pressure path
    /// every caller relies on.
    #[test]
    fn oversize_fd_vec_rejected() {
        let state = SurfaceShareState::new();
        let socket_path = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
        service.start().expect("service start");
        std::thread::sleep(std::time::Duration::from_millis(50));

        let stream = connect_to_surface_share_socket(&socket_path).expect("connect");

        // MAX_DMA_BUF_PLANES + 1 fds — one over the cap.
        let fds: Vec<RawFd> = (0..=MAX_DMA_BUF_PLANES)
            .map(|i| make_memfd_with(format!("plane-{}", i).as_bytes()))
            .collect();

        let check_in_req = serde_json::json!({
            "op": "check_in",
            "runtime_id": "test-runtime",
            "width": 640,
            "height": 480,
            "format": "Custom",
            "plane_sizes": vec![0u64; fds.len()],
            "plane_offsets": vec![0u64; fds.len()],
        });

        // The wire helper rejects with InvalidInput *before* any syscall,
        // without closing the caller-owned fds.
        let err = send_request_with_fds(&stream, &check_in_req, &fds, 0)
            .expect_err("oversize vec must be rejected");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);

        // Every caller fd is still valid; no leaks, no double-closes.
        for fd in &fds {
            let rc = unsafe { libc::fcntl(*fd, libc::F_GETFD) };
            assert!(rc >= 0, "caller fd {} must still be valid", fd);
            unsafe { libc::close(*fd) };
        }

        drop(stream);
        service.stop();
    }
}
