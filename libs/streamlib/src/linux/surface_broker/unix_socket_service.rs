// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-runtime Unix socket surface-sharing service.
//!
//! Each `StreamRuntime` owns one of these listening on a unique socket under
//! `$XDG_RUNTIME_DIR`. Polyglot subprocesses connect via `connect_to_broker`
//! / `send_request` (from [`streamlib_broker_client`]) and exchange DMA-BUF
//! fds over `SCM_RIGHTS`.

use std::io::Read;
use std::os::unix::io::RawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use streamlib_broker_client::{recv_message_with_fd, send_message_with_fd};

use super::state::SurfaceBrokerState;

pub struct UnixSocketSurfaceService {
    state: SurfaceBrokerState,
    socket_path: PathBuf,
    listener_thread: Option<thread::JoinHandle<()>>,
    shutdown_flag: Arc<std::sync::atomic::AtomicBool>,
}

impl UnixSocketSurfaceService {
    pub fn new(state: SurfaceBrokerState, socket_path: PathBuf) -> Self {
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
            "[Runtime broker] Unix socket surface service listening on {:?}",
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

        tracing::info!("[Runtime broker] Unix socket surface service stopped");
    }
}

impl Drop for UnixSocketSurfaceService {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_listener(
    listener: UnixListener,
    state: SurfaceBrokerState,
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
                        tracing::debug!("[Runtime broker] Client connection ended: {}", e);
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
                tracing::warn!("[Runtime broker] Unix socket accept error: {}", e);
                thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
}

fn handle_client_connection(
    mut stream: UnixStream,
    state: SurfaceBrokerState,
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

        let (json_bytes, received_fd) = recv_message_with_fd(&stream, msg_len)?;

        let request: serde_json::Value = serde_json::from_slice(&json_bytes).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("Invalid JSON: {}", e))
        })?;

        let op = request.get("op").and_then(|v| v.as_str()).unwrap_or("");

        let (response, reply_fd) = match op {
            "register" => handle_register(&state, &request, received_fd),
            "lookup" | "check_out" => handle_lookup(&state, &request),
            "unregister" | "release" => handle_unregister(&state, &request),
            "check_in" => handle_check_in(&state, &request, received_fd),
            _ => (
                serde_json::json!({"error": format!("unknown operation: {}", op)}),
                None,
            ),
        };

        if let Some(fd) = received_fd {
            unsafe { libc::close(fd) };
        }

        let response_bytes = serde_json::to_vec(&response).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to serialize response: {}", e),
            )
        })?;

        send_message_with_fd(&stream, &response_bytes, reply_fd)?;

        if let Some(fd) = reply_fd {
            unsafe { libc::close(fd) };
        }
    }
}

fn handle_register(
    state: &SurfaceBrokerState,
    request: &serde_json::Value,
    received_fd: Option<RawFd>,
) -> (serde_json::Value, Option<RawFd>) {
    let surface_id = match request.get("surface_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return (serde_json::json!({"error": "missing surface_id"}), None),
    };

    let runtime_id = request
        .get("runtime_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let dma_buf_fd = match received_fd {
        Some(fd) => fd,
        None => return (serde_json::json!({"error": "missing DMA-BUF fd"}), None),
    };

    let width = request.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let height = request.get("height").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let format = request.get("format").and_then(|v| v.as_str()).unwrap_or("unknown");
    let resource_type = request
        .get("resource_type")
        .and_then(|v| v.as_str())
        .unwrap_or("pixel_buffer");

    let dup_fd = unsafe { libc::dup(dma_buf_fd) };
    if dup_fd < 0 {
        return (
            serde_json::json!({"error": "failed to dup DMA-BUF fd"}),
            None,
        );
    }

    let success =
        state.register_surface(surface_id, runtime_id, dup_fd, width, height, format, resource_type);

    if success {
        tracing::debug!(
            "[Runtime broker] register: surface '{}' for runtime '{}' (fd {})",
            surface_id,
            runtime_id,
            dup_fd
        );
    } else {
        unsafe { libc::close(dup_fd) };
        tracing::warn!(
            "[Runtime broker] register: surface '{}' already exists",
            surface_id
        );
    }

    (serde_json::json!({"success": success}), None)
}

fn handle_lookup(
    state: &SurfaceBrokerState,
    request: &serde_json::Value,
) -> (serde_json::Value, Option<RawFd>) {
    let surface_id = match request.get("surface_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return (serde_json::json!({"error": "missing surface_id"}), None),
    };

    match state.get_surface_dma_buf_fd(surface_id) {
        Some(fd) => {
            let dup_fd = unsafe { libc::dup(fd) };
            if dup_fd < 0 {
                return (
                    serde_json::json!({"error": "failed to dup DMA-BUF fd"}),
                    None,
                );
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
                }),
                Some(dup_fd),
            )
        }
        None => (serde_json::json!({"error": "surface not found"}), None),
    }
}

fn handle_unregister(
    state: &SurfaceBrokerState,
    request: &serde_json::Value,
) -> (serde_json::Value, Option<RawFd>) {
    let surface_id = match request.get("surface_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return (serde_json::json!({"error": "missing surface_id"}), None),
    };

    let runtime_id = request
        .get("runtime_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let released = state.release_surface(surface_id, runtime_id);
    (serde_json::json!({"success": released}), None)
}

fn handle_check_in(
    state: &SurfaceBrokerState,
    request: &serde_json::Value,
    received_fd: Option<RawFd>,
) -> (serde_json::Value, Option<RawFd>) {
    let runtime_id = request
        .get("runtime_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let dma_buf_fd = match received_fd {
        Some(fd) => fd,
        None => return (serde_json::json!({"error": "missing DMA-BUF fd"}), None),
    };

    let width = request.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let height = request.get("height").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let format = request.get("format").and_then(|v| v.as_str()).unwrap_or("unknown");
    let resource_type = request
        .get("resource_type")
        .and_then(|v| v.as_str())
        .unwrap_or("pixel_buffer");

    let surface_id = uuid::Uuid::new_v4().to_string();

    let dup_fd = unsafe { libc::dup(dma_buf_fd) };
    if dup_fd < 0 {
        return (
            serde_json::json!({"error": "failed to dup DMA-BUF fd"}),
            None,
        );
    }

    let success =
        state.register_surface(&surface_id, runtime_id, dup_fd, width, height, format, resource_type);

    if !success {
        unsafe { libc::close(dup_fd) };
    }

    (serde_json::json!({"surface_id": surface_id}), None)
}

unsafe impl Send for UnixSocketSurfaceService {}
unsafe impl Sync for UnixSocketSurfaceService {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::FromRawFd;
    use streamlib_broker_client::{connect_to_broker, send_request};

    fn make_memfd_with(contents: &[u8]) -> RawFd {
        use std::io::{Seek, SeekFrom, Write};

        let name = std::ffi::CString::new("streamlib-runtime-broker-test").unwrap();
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
            "streamlib-runtime-broker-test-{}-{}.sock",
            std::process::id(),
            nanos
        ));
        p
    }

    #[test]
    fn check_in_check_out_roundtrip_preserves_fd_content() {
        let state = SurfaceBrokerState::new();
        let socket_path = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
        service.start().expect("service start");

        std::thread::sleep(std::time::Duration::from_millis(50));

        let stream = connect_to_broker(&socket_path).expect("connect");

        let pattern = b"streamlib-runtime-broker-test-fd-contents-0123456789";
        let send_fd = make_memfd_with(pattern);

        let check_in_req = serde_json::json!({
            "op": "check_in",
            "runtime_id": "test-runtime",
            "width": 640,
            "height": 480,
            "format": "Bgra32",
            "resource_type": "pixel_buffer",
        });
        let (check_in_resp, check_in_fd) =
            send_request(&stream, &check_in_req, Some(send_fd)).expect("check_in request");
        unsafe { libc::close(send_fd) };
        assert!(check_in_fd.is_none(), "check_in must not return an fd");
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
        let (check_out_resp, check_out_fd) =
            send_request(&stream, &check_out_req, None).expect("check_out request");
        assert_eq!(check_out_resp.get("width").and_then(|v| v.as_u64()), Some(640));
        assert_eq!(check_out_resp.get("height").and_then(|v| v.as_u64()), Some(480));
        assert_eq!(check_out_resp.get("format").and_then(|v| v.as_str()), Some("Bgra32"));
        let received_fd = check_out_fd.expect("check_out must return an fd");
        assert!(received_fd >= 0);
        let received = read_all_from_fd(received_fd);
        assert_eq!(received, pattern);

        let release_req = serde_json::json!({
            "op": "release",
            "surface_id": surface_id,
            "runtime_id": "test-runtime",
        });
        let _ = send_request(&stream, &release_req, None).expect("release request");

        drop(stream);
        service.stop();
    }

    #[test]
    fn check_out_unknown_surface_id_returns_error_no_fd() {
        let state = SurfaceBrokerState::new();
        let socket_path = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
        service.start().expect("service start");
        std::thread::sleep(std::time::Duration::from_millis(50));

        let stream = connect_to_broker(&socket_path).expect("connect");
        let req = serde_json::json!({
            "op": "check_out",
            "surface_id": "never-registered",
        });
        let (resp, fd) = send_request(&stream, &req, None).expect("check_out request");
        assert!(fd.is_none());
        assert!(resp.get("error").and_then(|v| v.as_str()).is_some());

        drop(stream);
        service.stop();
    }
}
