// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Unix socket surface service for the Linux broker.
//!
//! Provides a Unix domain socket listener that handles surface registration,
//! lookup, and lifecycle operations for cross-process DMA-BUF fd sharing.

use std::io::Read;
use std::os::unix::io::RawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;

use streamlib_broker_client::{recv_message_with_fd, send_message_with_fd};

use crate::state::BrokerState;

// Re-export the consumer-side wire helpers so existing call sites
// (`streamlib_broker::unix_socket_service::connect_to_broker`, etc.) keep
// working unchanged. See the [`streamlib_broker_client`] crate for the
// canonical implementations.
pub use streamlib_broker_client::{connect_to_broker, send_request};

enum ListenerSource {
    BindPath(PathBuf),
    Inherited(Option<UnixListener>),
}

/// Unix socket surface service for cross-process DMA-BUF sharing.
pub struct UnixSocketSurfaceService {
    state: BrokerState,
    source: ListenerSource,
    listener_thread: Option<thread::JoinHandle<()>>,
    shutdown_flag: Arc<AtomicBool>,
    active_count: Arc<AtomicUsize>,
}

impl UnixSocketSurfaceService {
    /// Create a new Unix socket surface service that will bind `socket_path`
    /// when [`start`](Self::start) is called.
    pub fn new(state: BrokerState, socket_path: PathBuf) -> Self {
        Self {
            state,
            source: ListenerSource::BindPath(socket_path),
            listener_thread: None,
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            active_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Create a new Unix socket surface service backed by a listener inherited
    /// from systemd socket activation (or equivalent). The service will not
    /// attempt to `bind` or to remove the socket file on stop — the listener's
    /// lifetime is the caller's responsibility (systemd for real activation;
    /// the test harness for simulated activation).
    pub fn with_inherited_listener(state: BrokerState, listener: UnixListener) -> Self {
        Self {
            state,
            source: ListenerSource::Inherited(Some(listener)),
            listener_thread: None,
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            active_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Start listening for connections.
    pub fn start(&mut self) -> Result<(), String> {
        let listener = match &mut self.source {
            ListenerSource::BindPath(socket_path) => {
                // Remove stale socket file if it exists
                if socket_path.exists() {
                    std::fs::remove_file(&*socket_path)
                        .map_err(|e| format!("Failed to remove stale socket: {}", e))?;
                }

                // Ensure parent directory exists
                if let Some(parent) = socket_path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("Failed to create socket directory: {}", e))?;
                }

                UnixListener::bind(&*socket_path).map_err(|e| {
                    format!("Failed to bind Unix socket at {:?}: {}", socket_path, e)
                })?
            }
            ListenerSource::Inherited(slot) => slot.take().ok_or_else(|| {
                "inherited listener already consumed (start() called twice)".to_string()
            })?,
        };

        // Set non-blocking so we can check the shutdown flag
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("Failed to set non-blocking: {}", e))?;

        let state = self.state.clone();
        let shutdown_flag = self.shutdown_flag.clone();
        let active_count = self.active_count.clone();

        let handle = thread::spawn(move || {
            run_listener(listener, state, shutdown_flag, active_count);
        });

        self.listener_thread = Some(handle);

        tracing::info!(
            "[Broker] Unix socket surface service listening ({})",
            match &self.source {
                ListenerSource::BindPath(p) => format!("bound: {:?}", p),
                ListenerSource::Inherited(_) => "inherited".to_string(),
            }
        );

        Ok(())
    }

    /// Stop the Unix socket service.
    pub fn stop(&mut self) {
        self.shutdown_flag.store(true, Ordering::Relaxed);

        if let Some(handle) = self.listener_thread.take() {
            let _ = handle.join();
        }

        // Only clean up the socket file if we bound it ourselves — for an
        // inherited listener the socket is owned by systemd (or the test
        // harness) and must outlive the daemon.
        if let ListenerSource::BindPath(socket_path) = &self.source
            && socket_path.exists()
        {
            let _ = std::fs::remove_file(socket_path);
        }

        tracing::info!("[Broker] Unix socket surface service stopped");
    }

    /// Current number of in-flight client connections.
    pub fn active_connection_count(&self) -> usize {
        self.active_count.load(Ordering::Relaxed)
    }

    /// Shared handle to the active-connection counter, for external idle-exit
    /// monitors.
    pub fn active_connection_count_arc(&self) -> Arc<AtomicUsize> {
        self.active_count.clone()
    }

    /// Get the socket path, if this service bound its own socket. Returns
    /// `None` for inherited listeners.
    pub fn socket_path(&self) -> Option<&Path> {
        match &self.source {
            ListenerSource::BindPath(p) => Some(p.as_path()),
            ListenerSource::Inherited(_) => None,
        }
    }
}

impl Drop for UnixSocketSurfaceService {
    fn drop(&mut self) {
        self.stop();
    }
}

/// RAII guard that increments the active-connection counter on construction
/// and decrements on drop.
struct ActiveConnectionGuard(Arc<AtomicUsize>);

impl ActiveConnectionGuard {
    fn new(count: Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::Relaxed);
        Self(count)
    }
}

impl Drop for ActiveConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Main listener loop.
fn run_listener(
    listener: UnixListener,
    state: BrokerState,
    shutdown_flag: Arc<AtomicBool>,
    active_count: Arc<AtomicUsize>,
) {
    loop {
        if shutdown_flag.load(Ordering::Relaxed) {
            break;
        }

        match listener.accept() {
            Ok((stream, _addr)) => {
                let state = state.clone();
                let active_count = active_count.clone();
                thread::spawn(move || {
                    let _guard = ActiveConnectionGuard::new(active_count);
                    if let Err(e) = handle_client_connection(stream, state) {
                        tracing::debug!("[Broker] Client connection ended: {}", e);
                    }
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // No pending connection, sleep briefly and retry
                thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => {
                if shutdown_flag.load(Ordering::Relaxed) {
                    break;
                }
                tracing::warn!("[Broker] Unix socket accept error: {}", e);
                thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
}

/// Handle a single client connection (multiple request/response rounds).
fn handle_client_connection(
    mut stream: UnixStream,
    state: BrokerState,
) -> Result<(), std::io::Error> {
    // Set blocking for message I/O
    stream.set_nonblocking(false)?;

    loop {
        // Read the message length prefix (4 bytes, big-endian)
        let mut len_buf = [0u8; 4];
        match stream.read_exact(&mut len_buf) {
            Ok(()) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                // Client disconnected
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

        // Try to receive the JSON payload and optional ancillary fd
        let (json_bytes, received_fd) = recv_message_with_fd(&stream, msg_len)?;

        // Parse JSON
        let request: serde_json::Value = serde_json::from_slice(&json_bytes).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Invalid JSON: {}", e),
            )
        })?;

        let op = request.get("op").and_then(|v| v.as_str()).unwrap_or("");

        let (response, reply_fd) = match op {
            "ping" => (serde_json::json!({"pong": true}), None),
            "register" => handle_register(&state, &request, received_fd),
            "lookup" => handle_lookup(&state, &request),
            "unregister" => handle_unregister(&state, &request),
            "check_in" => handle_check_in(&state, &request, received_fd),
            "check_out" => handle_check_out(&state, &request),
            "release" => handle_release(&state, &request),
            _ => (
                serde_json::json!({"error": format!("unknown operation: {}", op)}),
                None,
            ),
        };

        // Always close received fd after dispatch — handlers dup when they need to keep it
        if let Some(fd) = received_fd {
            unsafe { libc::close(fd) };
        }

        // Send response
        let response_bytes = serde_json::to_vec(&response).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to serialize response: {}", e),
            )
        })?;

        send_message_with_fd(&stream, &response_bytes, reply_fd)?;

        // Close the reply fd after sending (the recipient got a dup)
        if let Some(fd) = reply_fd {
            unsafe { libc::close(fd) };
        }
    }
}

// =============================================================================
// Message handlers
// =============================================================================

/// Handle register: client provides surface_id and DMA-BUF fd.
fn handle_register(
    state: &BrokerState,
    request: &serde_json::Value,
    received_fd: Option<RawFd>,
) -> (serde_json::Value, Option<RawFd>) {
    let surface_id = match request.get("surface_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => {
            return (serde_json::json!({"error": "missing surface_id"}), None);
        }
    };

    let runtime_id = request
        .get("runtime_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let dma_buf_fd = match received_fd {
        Some(fd) => fd,
        None => {
            return (serde_json::json!({"error": "missing DMA-BUF fd"}), None);
        }
    };

    let width = request.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let height = request.get("height").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let format = request
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let resource_type = request
        .get("resource_type")
        .and_then(|v| v.as_str())
        .unwrap_or("pixel_buffer");

    // Duplicate the fd so we own a copy (the received fd belongs to the message)
    let dup_fd = unsafe { libc::dup(dma_buf_fd) };
    if dup_fd < 0 {
        return (
            serde_json::json!({"error": "failed to dup DMA-BUF fd"}),
            None,
        );
    }

    let success = state.register_surface(
        surface_id,
        runtime_id,
        dup_fd,
        width,
        height,
        format,
        resource_type,
    );

    if success {
        tracing::debug!(
            "[Broker] Unix socket register: surface '{}' for runtime '{}' (fd {})",
            surface_id,
            runtime_id,
            dup_fd
        );
    } else {
        // Close the dup if we didn't register
        unsafe { libc::close(dup_fd) };
        tracing::warn!(
            "[Broker] Unix socket register: surface '{}' already exists",
            surface_id
        );
    }

    (serde_json::json!({"success": success}), None)
}

/// Handle lookup: return the DMA-BUF fd for a surface_id.
fn handle_lookup(
    state: &BrokerState,
    request: &serde_json::Value,
) -> (serde_json::Value, Option<RawFd>) {
    let surface_id = match request.get("surface_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return (serde_json::json!({"error": "missing surface_id"}), None),
    };

    match state.get_surface_dma_buf_fd(surface_id) {
        Some(fd) => {
            // Dup the fd for the client
            let dup_fd = unsafe { libc::dup(fd) };
            if dup_fd < 0 {
                return (
                    serde_json::json!({"error": "failed to dup DMA-BUF fd"}),
                    None,
                );
            }
            tracing::trace!(
                "[Broker] Unix socket lookup: returning fd {} for surface '{}'",
                dup_fd,
                surface_id
            );
            // Include surface metadata so the client can import the fd correctly
            let surfaces = state.get_surfaces();
            let metadata = surfaces.iter().find(|s| s.surface_id == surface_id);
            let (width, height, format, resource_type) = match metadata {
                Some(m) => (
                    m.width,
                    m.height,
                    m.format.as_str(),
                    m.resource_type.as_str(),
                ),
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
        None => {
            tracing::warn!(
                "[Broker] Unix socket lookup: surface '{}' not found",
                surface_id
            );
            (serde_json::json!({"error": "surface not found"}), None)
        }
    }
}

/// Handle unregister: remove a surface.
fn handle_unregister(
    state: &BrokerState,
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

    tracing::debug!(
        "[Broker] Unix socket unregister: surface '{}' released={}",
        surface_id,
        released
    );

    (serde_json::json!({"success": released}), None)
}

/// Handle check_in (legacy): broker generates surface_id.
fn handle_check_in(
    state: &BrokerState,
    request: &serde_json::Value,
    received_fd: Option<RawFd>,
) -> (serde_json::Value, Option<RawFd>) {
    let runtime_id = request
        .get("runtime_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let dma_buf_fd = match received_fd {
        Some(fd) => fd,
        None => {
            return (serde_json::json!({"error": "missing DMA-BUF fd"}), None);
        }
    };

    let width = request.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let height = request.get("height").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let format = request
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let resource_type = request
        .get("resource_type")
        .and_then(|v| v.as_str())
        .unwrap_or("pixel_buffer");

    // Generate UUID on broker side (legacy behavior)
    let surface_id = uuid::Uuid::new_v4().to_string();

    // Duplicate the fd
    let dup_fd = unsafe { libc::dup(dma_buf_fd) };
    if dup_fd < 0 {
        return (
            serde_json::json!({"error": "failed to dup DMA-BUF fd"}),
            None,
        );
    }

    let success = state.register_surface(
        &surface_id,
        runtime_id,
        dup_fd,
        width,
        height,
        format,
        resource_type,
    );

    if !success {
        unsafe { libc::close(dup_fd) };
    }

    tracing::debug!(
        "[Broker] Unix socket check_in: registered surface '{}' for runtime '{}' (fd {})",
        surface_id,
        runtime_id,
        dup_fd
    );

    (serde_json::json!({"surface_id": surface_id}), None)
}

/// Handle check_out: return the DMA-BUF fd for a surface_id.
fn handle_check_out(
    state: &BrokerState,
    request: &serde_json::Value,
) -> (serde_json::Value, Option<RawFd>) {
    // Same as lookup
    handle_lookup(state, request)
}

/// Handle release: unregister a surface (fire-and-forget).
fn handle_release(
    state: &BrokerState,
    request: &serde_json::Value,
) -> (serde_json::Value, Option<RawFd>) {
    handle_unregister(state, request)
}

// Safety: The service manages thread-safe BrokerState
unsafe impl Send for UnixSocketSurfaceService {}
unsafe impl Sync for UnixSocketSurfaceService {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::FromRawFd;

    /// Create an anonymous kernel fd (memfd) seeded with `contents`. The fd
    /// supports `lseek` + `read`, so we can verify that an fd received over
    /// SCM_RIGHTS still refers to the same kernel file object.
    fn make_memfd_with(contents: &[u8]) -> RawFd {
        use std::io::{Seek, SeekFrom, Write};

        let name = std::ffi::CString::new("streamlib-broker-test").unwrap();
        let fd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
        assert!(
            fd >= 0,
            "memfd_create failed: {}",
            std::io::Error::last_os_error()
        );
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.write_all(contents).expect("memfd write");
        file.seek(SeekFrom::Start(0)).expect("memfd rewind");
        // Leak the File wrapper so the raw fd stays open — caller owns it.
        let raw = {
            use std::os::unix::io::IntoRawFd;
            file.into_raw_fd()
        };
        raw
    }

    fn read_all_from_fd(fd: RawFd) -> Vec<u8> {
        use std::io::Read;

        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        // memfd_create starts read/write and non-sealed, but SCM_RIGHTS
        // delivers a duplicated fd whose offset is independent of the
        // sender's. Rewind so we read from the start regardless.
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(0)).expect("recv memfd rewind");
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).expect("recv memfd read");
        // Let the File drop close the fd.
        buf
    }

    fn tmp_socket_path() -> PathBuf {
        let mut p = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!(
            "streamlib-broker-test-{}-{}.sock",
            std::process::id(),
            nanos
        ));
        p
    }

    #[test]
    fn check_in_check_out_roundtrip_preserves_fd_content() {
        // Start a broker service in-process.
        let state = BrokerState::new();
        let socket_path = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state.clone(), socket_path.clone());
        service.start().expect("service start");

        // Give the listener a moment to accept.
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Client side: connect and check_in a memfd seeded with a pattern.
        let stream = connect_to_broker(&socket_path).expect("connect");

        let pattern = b"streamlib-broker-test-fd-contents-0123456789";
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
        // Close our copy of the sent fd — the broker dup'd it.
        unsafe { libc::close(send_fd) };
        assert!(check_in_fd.is_none(), "check_in must not return an fd");
        let surface_id = check_in_resp
            .get("surface_id")
            .and_then(|v| v.as_str())
            .expect("surface_id in response")
            .to_string();
        assert!(!surface_id.is_empty());

        // check_out the same surface_id — broker should return a dup of the
        // stored fd whose contents are byte-for-byte identical.
        let check_out_req = serde_json::json!({
            "op": "check_out",
            "surface_id": surface_id,
        });
        let (check_out_resp, check_out_fd) =
            send_request(&stream, &check_out_req, None).expect("check_out request");
        assert_eq!(
            check_out_resp.get("width").and_then(|v| v.as_u64()),
            Some(640)
        );
        assert_eq!(
            check_out_resp.get("height").and_then(|v| v.as_u64()),
            Some(480)
        );
        assert_eq!(
            check_out_resp.get("format").and_then(|v| v.as_str()),
            Some("Bgra32")
        );
        let received_fd = check_out_fd.expect("check_out must return an fd");
        assert!(received_fd >= 0);
        let received = read_all_from_fd(received_fd);
        assert_eq!(
            received, pattern,
            "SCM_RIGHTS should preserve the underlying memfd contents"
        );

        // release — fire-and-forget-shaped wire, still returns a JSON reply.
        let release_req = serde_json::json!({
            "op": "release",
            "surface_id": surface_id,
            "runtime_id": "test-runtime",
        });
        let (_release_resp, _) =
            send_request(&stream, &release_req, None).expect("release request");

        drop(stream);
        service.stop();
    }

    #[test]
    fn check_out_unknown_surface_id_returns_error_no_fd() {
        let state = BrokerState::new();
        let socket_path = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state.clone(), socket_path.clone());
        service.start().expect("service start");
        std::thread::sleep(std::time::Duration::from_millis(50));

        let stream = connect_to_broker(&socket_path).expect("connect");
        let req = serde_json::json!({
            "op": "check_out",
            "surface_id": "never-registered",
        });
        let (resp, fd) = send_request(&stream, &req, None).expect("check_out request");
        assert!(fd.is_none(), "no fd when surface missing");
        assert!(
            resp.get("error").and_then(|v| v.as_str()).is_some(),
            "missing-surface check_out must return an error payload"
        );

        drop(stream);
        service.stop();
    }

    /// `ping` returns `{"pong": true}` — used by `--probe` and the fixture
    /// script to confirm the daemon is responding on the socket. Keeping the
    /// shape stable matters: `scripts/streamlib_broker.sh` asserts the exact
    /// key name.
    #[test]
    fn ping_returns_pong() {
        let state = BrokerState::new();
        let socket_path = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state.clone(), socket_path.clone());
        service.start().expect("service start");
        std::thread::sleep(std::time::Duration::from_millis(50));

        let stream = connect_to_broker(&socket_path).expect("connect");
        let req = serde_json::json!({"op": "ping"});
        let (resp, fd) = send_request(&stream, &req, None).expect("ping request");
        assert!(fd.is_none(), "ping returns no fd");
        assert_eq!(resp.get("pong").and_then(|v| v.as_bool()), Some(true));

        drop(stream);
        service.stop();
    }

    /// Active-connection counter rises with in-flight clients and falls to
    /// zero once they disconnect. Drives the idle-exit logic in `main.rs`.
    #[test]
    fn active_connection_count_tracks_in_flight_clients() {
        let state = BrokerState::new();
        let socket_path = tmp_socket_path();
        let mut service = UnixSocketSurfaceService::new(state.clone(), socket_path.clone());
        service.start().expect("service start");
        std::thread::sleep(std::time::Duration::from_millis(50));

        assert_eq!(service.active_connection_count(), 0);

        let stream = connect_to_broker(&socket_path).expect("connect");
        // One round-trip so the server-side handler thread has been spawned
        // and the guard incremented.
        let (_resp, _fd) =
            send_request(&stream, &serde_json::json!({"op": "ping"}), None).expect("ping request");
        assert_eq!(service.active_connection_count(), 1);

        drop(stream);
        // The server-side handler returns from its read loop on UnexpectedEof
        // and drops the guard. Give the scheduler a moment.
        for _ in 0..50 {
            if service.active_connection_count() == 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(service.active_connection_count(), 0);

        service.stop();
    }

    /// When the service is handed a pre-bound listener (as in systemd socket
    /// activation), `start()` must use it directly — no bind attempt, no
    /// stale-socket removal — and `stop()` must leave the socket file alone
    /// because it's owned by the caller.
    #[test]
    fn inherited_listener_path_does_not_bind_or_unlink() {
        let socket_path = tmp_socket_path();
        // Bind ourselves first, as systemd would have done.
        let listener = UnixListener::bind(&socket_path).expect("pre-bind");

        let state = BrokerState::new();
        let mut service = UnixSocketSurfaceService::with_inherited_listener(state, listener);
        assert!(service.socket_path().is_none());
        service.start().expect("service start");
        std::thread::sleep(std::time::Duration::from_millis(50));

        // A client can still round-trip through the inherited listener.
        let stream = connect_to_broker(&socket_path).expect("connect");
        let (resp, _) =
            send_request(&stream, &serde_json::json!({"op": "ping"}), None).expect("ping");
        assert_eq!(resp.get("pong").and_then(|v| v.as_bool()), Some(true));
        drop(stream);

        service.stop();

        // The socket file must still exist — systemd (or, in this test, the
        // caller) owns its lifetime.
        assert!(
            socket_path.exists(),
            "inherited listener path must not be unlinked on stop"
        );
        let _ = std::fs::remove_file(&socket_path);
    }
}
