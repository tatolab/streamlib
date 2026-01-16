// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Python bindings for XPC connection (Phase 4 connection-based pattern).
//!
//! Provides XpcConnection class for subprocess-side XPC communication using
//! the broker-coordinated connection_id pattern.
//!
//! This module is designed for Python subprocesses to:
//! 1. Connect to the broker to retrieve host's XPC endpoint
//! 2. Establish direct XPC connection to the host processor
//! 3. Exchange frames via IOSurface/xpc_shmem

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

/// XPC connection state for tracking lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[pyclass(name = "XpcConnectionState")]
pub enum PyXpcConnectionState {
    /// Not connected yet.
    Disconnected,
    /// Connecting to broker/host.
    Connecting,
    /// Connected and ready for frame transfer.
    Connected,
    /// Connection error occurred.
    Error,
}

#[pymethods]
impl PyXpcConnectionState {
    fn __str__(&self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Error => "error",
        }
    }

    fn __repr__(&self) -> String {
        format!("XpcConnectionState.{}", self.__str__())
    }
}

/// XPC connection for Phase 4 connection-based pattern.
///
/// Used by Python subprocesses to connect to their host processor via
/// the broker-coordinated connection_id.
///
/// The connection flow is:
/// 1. Subprocess reads STREAMLIB_CONNECTION_ID from environment
/// 2. Subprocess calls ClientAlive via gRPC to signal it's alive
/// 3. Subprocess retrieves host's XPC endpoint from broker via XpcConnection.connect()
/// 4. XpcConnection.connect() uses the connection_id to get the endpoint
/// 5. Direct XPC connection is established for frame transfer
#[pyclass(name = "XpcConnection")]
pub struct PyXpcConnection {
    /// Connection ID from STREAMLIB_CONNECTION_ID env var.
    connection_id: String,
    /// Current connection state.
    state: PyXpcConnectionState,
    // TODO: Add xpc_connection_t when implementing actual XPC connection in Phase 4c
}

#[pymethods]
impl PyXpcConnection {
    /// Create a new XpcConnection from the connection_id.
    ///
    /// Args:
    ///     connection_id: The connection ID from STREAMLIB_CONNECTION_ID env var.
    ///
    /// Returns:
    ///     XpcConnection instance in disconnected state.
    #[new]
    fn new(connection_id: String) -> Self {
        tracing::debug!(
            "[PY XPC CONN] Created XpcConnection for connection_id: {}",
            connection_id
        );
        Self {
            connection_id,
            state: PyXpcConnectionState::Disconnected,
        }
    }

    /// Create an XpcConnection from environment variables.
    ///
    /// Reads STREAMLIB_CONNECTION_ID from the environment.
    ///
    /// Returns:
    ///     XpcConnection instance in disconnected state.
    ///
    /// Raises:
    ///     RuntimeError: If STREAMLIB_CONNECTION_ID is not set.
    #[staticmethod]
    fn from_env() -> PyResult<Self> {
        let connection_id = std::env::var("STREAMLIB_CONNECTION_ID").map_err(|_| {
            PyRuntimeError::new_err(
                "STREAMLIB_CONNECTION_ID environment variable not set. \
                 This should be set by the host processor when spawning the subprocess.",
            )
        })?;

        tracing::info!(
            "[PY XPC CONN] Created XpcConnection from env: connection_id={}",
            connection_id
        );

        Ok(Self::new(connection_id))
    }

    /// Get the connection ID.
    #[getter]
    fn connection_id(&self) -> &str {
        &self.connection_id
    }

    /// Get the current connection state.
    #[getter]
    fn state(&self) -> PyXpcConnectionState {
        self.state
    }

    /// Check if the connection is ready for frame transfer.
    fn is_connected(&self) -> bool {
        self.state == PyXpcConnectionState::Connected
    }

    /// Connect to the host processor's XPC endpoint.
    ///
    /// This retrieves the host's XPC endpoint from the broker via XPC
    /// (using get_endpoint_for_connection) and establishes a direct connection.
    ///
    /// Returns:
    ///     True if connection succeeded, False otherwise.
    ///
    /// Note: Phase 4b stub - actual XPC connection deferred to Phase 4c.
    #[cfg(target_os = "macos")]
    fn connect(&mut self) -> PyResult<bool> {
        tracing::info!(
            "[PY XPC CONN] connect() called for connection_id: {} (Phase 4b stub)",
            self.connection_id
        );

        self.state = PyXpcConnectionState::Connecting;

        // TODO: Phase 4c - Implement actual XPC endpoint retrieval and connection
        // 1. Connect to broker XPC service
        // 2. Send get_endpoint_for_connection message with connection_id
        // 3. Receive host's xpc_endpoint_t
        // 4. Create connection from endpoint
        // 5. Store connection for frame I/O

        // For now, mark as connected (stub for gRPC-only coordination testing)
        self.state = PyXpcConnectionState::Connected;

        tracing::info!(
            "[PY XPC CONN] connect() stub: marked as connected for connection_id: {}",
            self.connection_id
        );

        Ok(true)
    }

    #[cfg(not(target_os = "macos"))]
    fn connect(&mut self) -> PyResult<bool> {
        Err(PyRuntimeError::new_err("XPC is only available on macOS"))
    }

    /// Send ACK pong response to host.
    ///
    /// Called after receiving ACK ping from host to complete the handshake.
    ///
    /// Note: Phase 4b stub - actual ACK exchange deferred to Phase 4c.
    #[cfg(target_os = "macos")]
    fn send_ack_pong(&self) -> PyResult<()> {
        if self.state != PyXpcConnectionState::Connected {
            return Err(PyRuntimeError::new_err("Not connected"));
        }

        tracing::info!(
            "[PY XPC CONN] send_ack_pong() called for connection_id: {} (Phase 4b stub)",
            self.connection_id
        );

        // TODO: Phase 4c - Send ACK pong magic bytes (0x53 0x4C 0x41 "SLA")
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    fn send_ack_pong(&self) -> PyResult<()> {
        Err(PyRuntimeError::new_err("XPC is only available on macOS"))
    }

    /// Wait for ACK ping from host.
    ///
    /// Blocks until receiving the ACK ping from the host processor.
    ///
    /// Args:
    ///     timeout_ms: Timeout in milliseconds (default: 5000).
    ///
    /// Returns:
    ///     True if ping received, False if timeout.
    ///
    /// Note: Phase 4b stub - actual ACK exchange deferred to Phase 4c.
    #[cfg(target_os = "macos")]
    #[pyo3(signature = (timeout_ms=5000))]
    fn wait_for_ack_ping(&self, timeout_ms: u64) -> PyResult<bool> {
        if self.state != PyXpcConnectionState::Connected {
            return Err(PyRuntimeError::new_err("Not connected"));
        }

        tracing::info!(
            "[PY XPC CONN] wait_for_ack_ping() called: timeout={}ms (Phase 4b stub)",
            timeout_ms
        );

        // TODO: Phase 4c - Wait for ACK ping magic bytes (0x53 0x4C 0x50 "SLP")
        // For stub, return true immediately
        Ok(true)
    }

    #[cfg(not(target_os = "macos"))]
    #[pyo3(signature = (_timeout_ms=5000))]
    fn wait_for_ack_ping(&self, _timeout_ms: u64) -> PyResult<bool> {
        Err(PyRuntimeError::new_err("XPC is only available on macOS"))
    }

    /// Send a frame to the host processor via XPC.
    ///
    /// Args:
    ///     port_name: The port name for multiplexing (e.g., "video_out").
    ///     buffer: The pixel buffer to send.
    ///
    /// Returns:
    ///     Frame ID assigned to this frame.
    ///
    /// Note: Phase 4b stub - actual frame I/O deferred to Phase 4c.
    #[cfg(target_os = "macos")]
    fn send_frame(
        &self,
        port_name: &str,
        _buffer: &crate::pixel_buffer_binding::PyRhiPixelBuffer,
    ) -> PyResult<u64> {
        if self.state != PyXpcConnectionState::Connected {
            return Err(PyRuntimeError::new_err("Not connected"));
        }

        tracing::trace!(
            "[PY XPC CONN] send_frame() called: port='{}' (Phase 4b stub)",
            port_name
        );

        // TODO: Phase 4c - Implement actual frame send via XPC
        // 1. Export IOSurface to XPC handle
        // 2. Create XPC message with port_name and handle
        // 3. Send via xpc_connection_send_message
        // 4. Return frame_id

        Ok(0) // Stub frame_id
    }

    #[cfg(not(target_os = "macos"))]
    fn send_frame(
        &self,
        _port_name: &str,
        _buffer: &crate::pixel_buffer_binding::PyRhiPixelBuffer,
    ) -> PyResult<u64> {
        Err(PyRuntimeError::new_err("XPC is only available on macOS"))
    }

    /// Receive a frame from the host processor via XPC.
    ///
    /// Args:
    ///     port_name: The port name to receive from (e.g., "video_in").
    ///     timeout_ms: Timeout in milliseconds (default: 5000).
    ///
    /// Returns:
    ///     Tuple of (frame_id, PyRhiPixelBuffer) or None if timeout.
    ///
    /// Note: Phase 4b stub - actual frame I/O deferred to Phase 4c.
    #[cfg(target_os = "macos")]
    #[pyo3(signature = (port_name, timeout_ms=5000))]
    fn recv_frame(
        &self,
        port_name: &str,
        timeout_ms: u64,
    ) -> PyResult<Option<(u64, crate::pixel_buffer_binding::PyRhiPixelBuffer)>> {
        if self.state != PyXpcConnectionState::Connected {
            return Err(PyRuntimeError::new_err("Not connected"));
        }

        tracing::trace!(
            "[PY XPC CONN] recv_frame() called: port='{}' timeout={}ms (Phase 4b stub)",
            port_name,
            timeout_ms
        );

        // TODO: Phase 4c - Implement actual frame receive via XPC
        // 1. Wait for XPC message with matching port_name
        // 2. Import IOSurface from XPC handle
        // 3. Create PyRhiPixelBuffer
        // 4. Return (frame_id, buffer)

        Ok(None) // Stub - no frame
    }

    #[cfg(not(target_os = "macos"))]
    #[pyo3(signature = (_port_name, _timeout_ms=5000))]
    fn recv_frame(
        &self,
        _port_name: &str,
        _timeout_ms: u64,
    ) -> PyResult<Option<(u64, crate::pixel_buffer_binding::PyRhiPixelBuffer)>> {
        Err(PyRuntimeError::new_err("XPC is only available on macOS"))
    }

    /// Close the XPC connection.
    fn close(&mut self) {
        tracing::info!(
            "[PY XPC CONN] close() called for connection_id: {}",
            self.connection_id
        );

        // TODO: Phase 4c - Release XPC connection resources
        self.state = PyXpcConnectionState::Disconnected;
    }
}

impl Drop for PyXpcConnection {
    fn drop(&mut self) {
        if self.state == PyXpcConnectionState::Connected {
            tracing::debug!(
                "[PY XPC CONN] Dropping connected XpcConnection: {}",
                self.connection_id
            );
            self.close();
        }
    }
}
