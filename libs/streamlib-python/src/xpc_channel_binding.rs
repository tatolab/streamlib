// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Python bindings for XPC frame channel.
//!
//! Provides XPC-based IOSurface sharing for Python subprocesses on macOS.
//! Uses the new broker-based XpcChannel API for direct runtime-subprocess connections.

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

#[cfg(target_os = "macos")]
use std::ffi::c_void;
#[cfg(target_os = "macos")]
use std::time::Duration;

#[cfg(target_os = "macos")]
use streamlib::core::subprocess_rhi::{SubprocessRhiChannel, SubprocessRhiFrameTransport};
#[cfg(target_os = "macos")]
use streamlib::XpcChannel;
#[cfg(target_os = "macos")]
use streamlib::XpcFrameTransport;

/// XPC frame channel for bidirectional IOSurface frame transfer.
///
/// Uses the broker-based pattern for direct runtime-subprocess connections:
/// - Runtime creates anonymous listener and registers with broker
/// - Subprocess connects via broker endpoint
/// - Direct XPC connection for zero-copy IOSurface sharing
#[pyclass(name = "XpcFrameChannel")]
pub struct PyXpcFrameChannel {
    #[cfg(target_os = "macos")]
    inner: XpcChannel,
    #[cfg(target_os = "macos")]
    frame_id_counter: std::sync::atomic::AtomicU64,
}

#[pymethods]
impl PyXpcFrameChannel {
    /// Create an XPC channel as the runtime/host side.
    ///
    /// This creates an anonymous XPC listener and registers its endpoint with
    /// the broker service, allowing subprocesses to connect via `connect()`.
    ///
    /// Args:
    ///     runtime_id: Unique identifier for this runtime (used for broker lookup).
    ///
    /// Returns:
    ///     XpcFrameChannel instance as the runtime host.
    #[staticmethod]
    #[cfg(target_os = "macos")]
    fn create_host(runtime_id: &str) -> PyResult<Self> {
        tracing::trace!(
            "[PY XPC HOST] create_host called: runtime_id='{}' pid={}",
            runtime_id,
            std::process::id()
        );

        match XpcChannel::create_as_runtime(runtime_id) {
            Ok(channel) => {
                tracing::info!(
                    "[PY XPC HOST] SUCCESS: created channel for runtime_id='{}' pid={}",
                    runtime_id,
                    std::process::id()
                );
                Ok(Self {
                    inner: channel,
                    frame_id_counter: std::sync::atomic::AtomicU64::new(0),
                })
            }
            Err(e) => {
                tracing::error!(
                    "[PY XPC HOST] FAILED: runtime_id='{}' error='{}' pid={}",
                    runtime_id,
                    e,
                    std::process::id()
                );
                Err(PyRuntimeError::new_err(format!(
                    "XPC create_host failed: {}",
                    e
                )))
            }
        }
    }

    #[staticmethod]
    #[cfg(not(target_os = "macos"))]
    fn create_host(_runtime_id: &str) -> PyResult<Self> {
        Err(PyRuntimeError::new_err("XPC is only available on macOS"))
    }

    /// Connect to an XPC channel as a subprocess/client.
    ///
    /// This queries the broker for the runtime's endpoint and creates a direct
    /// XPC connection to the runtime process.
    ///
    /// Args:
    ///     runtime_id: The runtime identifier to connect to.
    ///
    /// Returns:
    ///     XpcFrameChannel instance connected to the runtime.
    #[staticmethod]
    #[cfg(target_os = "macos")]
    fn connect(runtime_id: &str) -> PyResult<Self> {
        tracing::trace!(
            "[PY XPC CLIENT] connect called: runtime_id='{}' pid={}",
            runtime_id,
            std::process::id()
        );

        match XpcChannel::connect_as_subprocess(runtime_id) {
            Ok(channel) => {
                tracing::info!(
                    "[PY XPC CLIENT] SUCCESS: connected to runtime_id='{}' pid={}",
                    runtime_id,
                    std::process::id()
                );
                Ok(Self {
                    inner: channel,
                    frame_id_counter: std::sync::atomic::AtomicU64::new(0),
                })
            }
            Err(e) => {
                tracing::error!(
                    "[PY XPC CLIENT] FAILED: runtime_id='{}' error='{}' pid={}",
                    runtime_id,
                    e,
                    std::process::id()
                );
                Err(PyRuntimeError::new_err(format!(
                    "XPC connect failed: {}",
                    e
                )))
            }
        }
    }

    #[staticmethod]
    #[cfg(not(target_os = "macos"))]
    fn connect(_runtime_id: &str) -> PyResult<Self> {
        Err(PyRuntimeError::new_err("XPC is only available on macOS"))
    }

    /// Get the runtime ID this channel is connected to.
    #[cfg(target_os = "macos")]
    fn runtime_id(&self) -> String {
        self.inner.runtime_id.clone()
    }

    #[cfg(not(target_os = "macos"))]
    fn runtime_id(&self) -> String {
        String::new()
    }

    /// Check if the channel is connected.
    #[cfg(target_os = "macos")]
    fn is_connected(&self) -> bool {
        self.inner.is_connected()
    }

    #[cfg(not(target_os = "macos"))]
    fn is_connected(&self) -> bool {
        false
    }

    /// Import an IOSurface from the XPC channel.
    ///
    /// This receives the next frame from the XPC channel and imports the
    /// IOSurface from the XPC object. The frame_id returned should match
    /// the expected xpc_object_id.
    ///
    /// Args:
    ///     xpc_object_id: The expected frame ID (for logging/validation).
    ///     width: Expected width in pixels.
    ///     height: Expected height in pixels.
    ///     format: Pixel format string (e.g., "bgra32").
    ///     timeout_ms: Timeout in milliseconds (default: 5000).
    ///
    /// Returns:
    ///     PyRhiPixelBuffer wrapping the imported IOSurface.
    #[cfg(target_os = "macos")]
    #[pyo3(signature = (xpc_object_id, width, height, format, timeout_ms=5000))]
    fn import_iosurface(
        &self,
        xpc_object_id: u64,
        width: u32,
        height: u32,
        format: &str,
        timeout_ms: u64,
    ) -> PyResult<crate::pixel_buffer_binding::PyRhiPixelBuffer> {
        use streamlib::core::rhi::{RhiPixelBuffer, RhiPixelBufferRef};

        tracing::trace!(
            "[PY XPC IMPORT] import_iosurface called: expected_id={} {}x{} {} timeout={}ms pid={}",
            xpc_object_id,
            width,
            height,
            format,
            timeout_ms,
            std::process::id()
        );

        // Receive frame from XPC channel
        let timeout = Duration::from_millis(timeout_ms);
        let (handle, frame_id) = match self.inner.recv_frame(timeout) {
            Ok(result) => {
                tracing::trace!(
                    "[PY XPC IMPORT] recv_frame SUCCESS: frame_id={} pid={}",
                    result.1,
                    std::process::id()
                );
                result
            }
            Err(e) => {
                tracing::error!(
                    "[PY XPC IMPORT] recv_frame FAILED: error='{}' pid={}",
                    e,
                    std::process::id()
                );
                return Err(PyRuntimeError::new_err(format!(
                    "XPC recv_frame failed: {}",
                    e
                )));
            }
        };

        // Import IOSurface from the handle
        let surface = match XpcFrameTransport::import_iosurface(handle) {
            Ok(s) => {
                tracing::trace!(
                    "[PY XPC IMPORT] import_iosurface SUCCESS: surface={:p} pid={}",
                    s,
                    std::process::id()
                );
                s
            }
            Err(e) => {
                tracing::error!(
                    "[PY XPC IMPORT] import_iosurface FAILED: error='{}' pid={}",
                    e,
                    std::process::id()
                );
                return Err(PyRuntimeError::new_err(format!(
                    "Failed to import IOSurface: {}",
                    e
                )));
            }
        };

        // Create RhiPixelBufferRef from the IOSurfaceRef
        let buffer_ref = unsafe { RhiPixelBufferRef::from_iosurface_ref(surface as *mut c_void) }
            .map_err(|e| {
            tracing::error!(
                "[PY XPC IMPORT] from_iosurface_ref FAILED: surface={:p} error='{}' pid={}",
                surface,
                e,
                std::process::id()
            );
            PyRuntimeError::new_err(format!("Failed to create buffer from IOSurface: {}", e))
        })?;

        // Wrap in RhiPixelBuffer and PyRhiPixelBuffer
        let buffer = RhiPixelBuffer::new(buffer_ref);

        tracing::info!(
            "[PY XPC IMPORT] SUCCESS: frame_id={} (expected={}) {}x{} {} pid={}",
            frame_id,
            xpc_object_id,
            width,
            height,
            format,
            std::process::id()
        );

        Ok(crate::pixel_buffer_binding::PyRhiPixelBuffer::new(buffer))
    }

    #[cfg(not(target_os = "macos"))]
    #[pyo3(signature = (_xpc_object_id, _width, _height, _format, _timeout_ms=5000))]
    fn import_iosurface(
        &self,
        _xpc_object_id: u64,
        _width: u32,
        _height: u32,
        _format: &str,
        _timeout_ms: u64,
    ) -> PyResult<crate::pixel_buffer_binding::PyRhiPixelBuffer> {
        Err(PyRuntimeError::new_err("XPC is only available on macOS"))
    }

    /// Export and send an IOSurface via XPC to the connected peer.
    ///
    /// This sends the buffer's IOSurface to the other end of the XPC connection.
    /// Used by Python subprocess to send processed frames back to the Rust host.
    ///
    /// Args:
    ///     buffer: PixelBuffer to export and send.
    ///
    /// Returns:
    ///     The frame_id assigned to this frame (include in IPC message).
    #[cfg(target_os = "macos")]
    fn export_iosurface(
        &self,
        buffer: &crate::pixel_buffer_binding::PyRhiPixelBuffer,
    ) -> PyResult<u64> {
        use std::sync::atomic::Ordering;

        // Get the IOSurfaceRef from the buffer
        let iosurface = buffer
            .inner()
            .buffer_ref()
            .iosurface_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Buffer is not backed by IOSurface"))?;

        // Generate unique frame_id
        let frame_id = self.frame_id_counter.fetch_add(1, Ordering::SeqCst);

        // Export IOSurface to XPC handle
        let handle = XpcFrameTransport::export_iosurface(iosurface as *mut c_void)
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to export IOSurface: {}", e)))?;

        // Send via XPC channel
        self.inner
            .send_frame(handle, frame_id)
            .map_err(|e| PyRuntimeError::new_err(format!("XPC send_frame failed: {}", e)))?;

        tracing::debug!(
            "[PY XPC EXPORT] sent frame_id={} IOSurface {:?} (pid={})",
            frame_id,
            iosurface,
            std::process::id()
        );

        Ok(frame_id)
    }

    #[cfg(not(target_os = "macos"))]
    fn export_iosurface(
        &self,
        _buffer: &crate::pixel_buffer_binding::PyRhiPixelBuffer,
    ) -> PyResult<u64> {
        Err(PyRuntimeError::new_err("XPC is only available on macOS"))
    }
}

// Make inner field accessible for macOS
#[cfg(target_os = "macos")]
impl PyXpcFrameChannel {
    /// Get access to the inner XpcChannel (for host_processor.rs integration).
    pub fn inner(&self) -> &XpcChannel {
        &self.inner
    }
}
