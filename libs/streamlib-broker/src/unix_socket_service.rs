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
use std::thread;

use crate::state::BrokerState;

/// Unix socket surface service for cross-process DMA-BUF sharing.
pub struct UnixSocketSurfaceService {
    state: BrokerState,
    socket_path: PathBuf,
    listener_thread: Option<thread::JoinHandle<()>>,
    shutdown_flag: Arc<std::sync::atomic::AtomicBool>,
}

impl UnixSocketSurfaceService {
    /// Create a new Unix socket surface service.
    pub fn new(state: BrokerState, socket_path: PathBuf) -> Self {
        Self {
            state,
            socket_path,
            listener_thread: None,
            shutdown_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Start listening for connections.
    pub fn start(&mut self) -> Result<(), String> {
        // Remove stale socket file if it exists
        if self.socket_path.exists() {
            std::fs::remove_file(&self.socket_path)
                .map_err(|e| format!("Failed to remove stale socket: {}", e))?;
        }

        // Ensure parent directory exists
        if let Some(parent) = self.socket_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create socket directory: {}", e))?;
        }

        let listener = UnixListener::bind(&self.socket_path)
            .map_err(|e| format!("Failed to bind Unix socket at {:?}: {}", self.socket_path, e))?;

        // Set non-blocking so we can check the shutdown flag
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
            "[Broker] Unix socket surface service listening on {:?}",
            self.socket_path
        );

        Ok(())
    }

    /// Stop the Unix socket service.
    pub fn stop(&mut self) {
        self.shutdown_flag
            .store(true, std::sync::atomic::Ordering::Relaxed);

        if let Some(handle) = self.listener_thread.take() {
            let _ = handle.join();
        }

        // Clean up socket file
        if self.socket_path.exists() {
            let _ = std::fs::remove_file(&self.socket_path);
        }

        tracing::info!("[Broker] Unix socket surface service stopped");
    }

    /// Get the socket path.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for UnixSocketSurfaceService {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Main listener loop.
fn run_listener(
    listener: UnixListener,
    state: BrokerState,
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
                        tracing::debug!("[Broker] Client connection ended: {}", e);
                    }
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // No pending connection, sleep briefly and retry
                thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => {
                if shutdown_flag.load(std::sync::atomic::Ordering::Relaxed) {
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
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("Invalid JSON: {}", e))
        })?;

        let op = request
            .get("op")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let (response, reply_fd) = match op {
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

        // Close received fd if it wasn't consumed
        if let Some(fd) = received_fd {
            if op != "register" && op != "check_in" {
                unsafe { libc::close(fd) };
            }
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
// SCM_RIGHTS fd passing helpers
// =============================================================================

/// Receive a length-prefixed message with optional SCM_RIGHTS fd.
fn recv_message_with_fd(
    stream: &UnixStream,
    msg_len: usize,
) -> Result<(Vec<u8>, Option<RawFd>), std::io::Error> {
    use std::os::unix::io::AsRawFd;

    let mut buf = vec![0u8; msg_len];

    // Control message buffer for SCM_RIGHTS (one fd)
    let mut cmsg_buf = [0u8; unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) }
        as usize];

    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: msg_len,
    };

    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = cmsg_buf.len();

    let n = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut msg, 0) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if n == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "Connection closed",
        ));
    }

    // If we didn't get the full message, read the remainder with plain read
    let mut total_read = n as usize;
    while total_read < msg_len {
        let remaining = &mut buf[total_read..];
        let n = unsafe {
            libc::read(
                stream.as_raw_fd(),
                remaining.as_mut_ptr() as *mut libc::c_void,
                remaining.len(),
            )
        };
        if n <= 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Connection closed during message read",
            ));
        }
        total_read += n as usize;
    }

    // Extract fd from control message if present
    let mut received_fd = None;
    let mut cmsg_ptr = unsafe { libc::CMSG_FIRSTHDR(&msg) };
    while !cmsg_ptr.is_null() {
        let cmsg = unsafe { &*cmsg_ptr };
        if cmsg.cmsg_level == libc::SOL_SOCKET && cmsg.cmsg_type == libc::SCM_RIGHTS {
            let fd_ptr = unsafe { libc::CMSG_DATA(cmsg_ptr) } as *const RawFd;
            received_fd = Some(unsafe { *fd_ptr });
        }
        cmsg_ptr = unsafe { libc::CMSG_NXTHDR(&msg, cmsg_ptr) };
    }

    Ok((buf, received_fd))
}

/// Send a length-prefixed message with optional SCM_RIGHTS fd.
fn send_message_with_fd(
    stream: &UnixStream,
    data: &[u8],
    fd: Option<RawFd>,
) -> Result<(), std::io::Error> {
    use std::os::unix::io::AsRawFd;

    // First send the length prefix
    let len_bytes = (data.len() as u32).to_be_bytes();
    let mut len_iov = libc::iovec {
        iov_base: len_bytes.as_ptr() as *mut libc::c_void,
        iov_len: 4,
    };
    let mut len_msg: libc::msghdr = unsafe { std::mem::zeroed() };
    len_msg.msg_iov = &mut len_iov;
    len_msg.msg_iovlen = 1;

    let n = unsafe { libc::sendmsg(stream.as_raw_fd(), &len_msg, 0) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }

    // Then send the data payload with optional fd
    let mut iov = libc::iovec {
        iov_base: data.as_ptr() as *mut libc::c_void,
        iov_len: data.len(),
    };

    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;

    let mut cmsg_buf = [0u8; unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) }
        as usize];

    if let Some(send_fd) = fd {
        msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = cmsg_buf.len();

        let cmsg_ptr = unsafe { libc::CMSG_FIRSTHDR(&msg) };
        if !cmsg_ptr.is_null() {
            unsafe {
                (*cmsg_ptr).cmsg_level = libc::SOL_SOCKET;
                (*cmsg_ptr).cmsg_type = libc::SCM_RIGHTS;
                (*cmsg_ptr).cmsg_len =
                    libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as usize;
                let fd_ptr = libc::CMSG_DATA(cmsg_ptr) as *mut RawFd;
                *fd_ptr = send_fd;
            }
            msg.msg_controllen =
                unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) } as usize;
        }
    }

    let n = unsafe { libc::sendmsg(stream.as_raw_fd(), &msg, 0) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(())
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
            if let Some(fd) = received_fd {
                unsafe { libc::close(fd) };
            }
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

    let width = request
        .get("width")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let height = request
        .get("height")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let format = request
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    // Duplicate the fd so we own a copy (the received fd belongs to the message)
    let dup_fd = unsafe { libc::dup(dma_buf_fd) };
    if dup_fd < 0 {
        return (
            serde_json::json!({"error": "failed to dup DMA-BUF fd"}),
            None,
        );
    }

    let success = state.register_surface(surface_id, runtime_id, dup_fd, width, height, format);

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
            (serde_json::json!({"surface_id": surface_id}), Some(dup_fd))
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

    let width = request
        .get("width")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let height = request
        .get("height")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let format = request
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

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

    let success = state.register_surface(&surface_id, runtime_id, dup_fd, width, height, format);

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

// =============================================================================
// Client-side helpers (used by SurfaceStore)
// =============================================================================

/// Connect to the broker's Unix socket.
pub fn connect_to_broker(socket_path: &Path) -> Result<UnixStream, std::io::Error> {
    UnixStream::connect(socket_path)
}

/// Send a request and receive a response from the broker.
pub fn send_request(
    stream: &UnixStream,
    request: &serde_json::Value,
    fd: Option<RawFd>,
) -> Result<(serde_json::Value, Option<RawFd>), std::io::Error> {
    let request_bytes = serde_json::to_vec(request).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Failed to serialize request: {}", e),
        )
    })?;

    // Send request with optional fd
    send_message_with_fd(stream, &request_bytes, fd)?;

    // Read response length prefix
    let mut len_buf = [0u8; 4];
    {
        use std::os::unix::io::AsRawFd;
        let mut total = 0;
        while total < 4 {
            let n = unsafe {
                libc::read(
                    stream.as_raw_fd(),
                    len_buf[total..].as_mut_ptr() as *mut libc::c_void,
                    4 - total,
                )
            };
            if n <= 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Failed to read response length",
                ));
            }
            total += n as usize;
        }
    }
    let response_len = u32::from_be_bytes(len_buf) as usize;

    // Read response with optional fd
    let (response_bytes, response_fd) = recv_message_with_fd(stream, response_len)?;

    let response: serde_json::Value = serde_json::from_slice(&response_bytes).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Invalid JSON response: {}", e),
        )
    })?;

    Ok((response, response_fd))
}

// Safety: The service manages thread-safe BrokerState
unsafe impl Send for UnixSocketSurfaceService {}
unsafe impl Sync for UnixSocketSurfaceService {}
