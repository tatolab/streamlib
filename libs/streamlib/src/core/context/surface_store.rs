// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Surface Store for cross-process GPU surface sharing.
//!
//! Provides check-in/check-out semantics for IOSurfaces via the broker's XPC service.
//! Surfaces are cached locally after first checkout to minimize XPC overhead.

use std::collections::HashMap;
use std::ffi::CString;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::rhi::RhiPixelBuffer;
use crate::core::{Result, StreamError};

#[cfg(target_os = "macos")]
use crate::apple::xpc_ffi::*;

/// Surface metadata stored alongside the cached pixel buffer.
#[derive(Debug, Clone)]
pub struct CachedSurface {
    /// The resolved pixel buffer.
    pub pixel_buffer: RhiPixelBuffer,
    /// Number of times this surface has been checked out.
    pub checkout_count: u64,
}

/// Local cache for resolved surfaces.
struct SurfaceCache {
    /// Map from surface ID to cached pixel buffer.
    surfaces: HashMap<String, CachedSurface>,
}

impl SurfaceCache {
    fn new() -> Self {
        Self {
            surfaces: HashMap::new(),
        }
    }

    fn get(&self, surface_id: &str) -> Option<&CachedSurface> {
        self.surfaces.get(surface_id)
    }

    fn insert(&mut self, surface_id: String, pixel_buffer: RhiPixelBuffer) {
        self.surfaces.insert(
            surface_id,
            CachedSurface {
                pixel_buffer,
                checkout_count: 1,
            },
        );
    }

    fn increment_checkout(&mut self, surface_id: &str) {
        if let Some(cached) = self.surfaces.get_mut(surface_id) {
            cached.checkout_count += 1;
        }
    }

    fn clear(&mut self) {
        self.surfaces.clear();
    }

    fn surface_ids(&self) -> Vec<String> {
        self.surfaces.keys().cloned().collect()
    }
}

/// Reverse lookup from pixel buffer identity to surface ID.
struct CheckedInSurfaces {
    /// Map from IOSurface ID (from IOSurfaceGetID) to surface store ID.
    iosurface_id_to_surface_id: HashMap<u32, String>,
}

impl CheckedInSurfaces {
    fn new() -> Self {
        Self {
            iosurface_id_to_surface_id: HashMap::new(),
        }
    }

    fn get_surface_id(&self, iosurface_id: u32) -> Option<&String> {
        self.iosurface_id_to_surface_id.get(&iosurface_id)
    }

    fn insert(&mut self, iosurface_id: u32, surface_id: String) {
        self.iosurface_id_to_surface_id
            .insert(iosurface_id, surface_id);
    }

    fn clear(&mut self) {
        self.iosurface_id_to_surface_id.clear();
    }

    fn surface_ids(&self) -> Vec<String> {
        self.iosurface_id_to_surface_id.values().cloned().collect()
    }
}

/// Surface store client for cross-process GPU surface sharing.
///
/// Connects to the broker's XPC service to exchange mach ports for surface IDs.
/// Caches resolved surfaces locally to minimize XPC round-trips.
#[derive(Clone)]
pub struct SurfaceStore {
    inner: Arc<SurfaceStoreInner>,
}

struct SurfaceStoreInner {
    /// XPC connection to the broker (macOS only).
    #[cfg(target_os = "macos")]
    connection: Mutex<Option<xpc_connection_t>>,

    /// Local cache of checked-out surfaces (surface_id -> pixel_buffer).
    cache: Mutex<SurfaceCache>,

    /// Reverse lookup for checked-in surfaces (iosurface_id -> surface_id).
    checked_in: Mutex<CheckedInSurfaces>,

    /// The XPC service name to connect to.
    service_name: String,

    /// Runtime ID for tracking which surfaces belong to this runtime.
    runtime_id: String,
}

impl SurfaceStore {
    /// Create a new surface store (not yet connected).
    pub fn new(service_name: String, runtime_id: String) -> Self {
        Self {
            inner: Arc::new(SurfaceStoreInner {
                #[cfg(target_os = "macos")]
                connection: Mutex::new(None),
                cache: Mutex::new(SurfaceCache::new()),
                checked_in: Mutex::new(CheckedInSurfaces::new()),
                service_name,
                runtime_id,
            }),
        }
    }

    /// Connect to the broker's XPC service.
    ///
    /// This should be called during runtime.start().
    #[cfg(target_os = "macos")]
    pub fn connect(&self) -> Result<()> {
        let service_name = CString::new(self.inner.service_name.as_str())
            .map_err(|e| StreamError::Configuration(format!("Invalid XPC service name: {}", e)))?;

        let connection = unsafe {
            xpc_connection_create_mach_service(
                service_name.as_ptr(),
                std::ptr::null_mut(), // default queue
                0,                    // no special flags
            )
        };

        if connection.is_null() {
            return Err(StreamError::Configuration(format!(
                "Failed to create XPC connection to '{}'",
                self.inner.service_name
            )));
        }

        // Set up a minimal event handler (required before resume)
        // For now, we use synchronous send/reply, so the handler just logs errors
        unsafe {
            // Create a simple block that does nothing (we use sync calls)
            // In a full implementation, this would handle connection errors
            xpc_connection_set_event_handler(connection, std::ptr::null_mut());
            xpc_connection_resume(connection);
        }

        *self.inner.connection.lock() = Some(connection);

        tracing::info!(
            "SurfaceStore: Connected to XPC service '{}'",
            self.inner.service_name
        );

        Ok(())
    }

    /// Disconnect from the broker and release all surfaces.
    ///
    /// This should be called during runtime.stop().
    #[cfg(target_os = "macos")]
    pub fn disconnect(&self) -> Result<()> {
        // Release all checked-in surfaces from the broker
        let surface_ids = self.inner.checked_in.lock().surface_ids();
        for surface_id in surface_ids {
            if let Err(e) = self.release_from_broker(&surface_id) {
                tracing::warn!(
                    "SurfaceStore: Failed to release surface '{}': {}",
                    surface_id,
                    e
                );
            }
        }

        // Clear local state
        self.inner.cache.lock().clear();
        self.inner.checked_in.lock().clear();

        // Cancel the XPC connection
        if let Some(connection) = self.inner.connection.lock().take() {
            unsafe {
                xpc_connection_cancel(connection);
            }
        }

        tracing::info!("SurfaceStore: Disconnected from XPC service");
        Ok(())
    }

    /// Check in a pixel buffer, returning a surface ID.
    ///
    /// If this pixel buffer was already checked in, returns the existing ID.
    /// Otherwise, sends the mach port to the broker and receives a new ID.
    #[cfg(target_os = "macos")]
    pub fn check_in(&self, pixel_buffer: &RhiPixelBuffer) -> Result<String> {
        use crate::apple::corevideo_ffi::IOSurfaceGetID;

        // Get the IOSurface ID for deduplication
        let pixel_buffer_ref = pixel_buffer.buffer_ref();
        let iosurface = pixel_buffer_ref.iosurface_ref().ok_or_else(|| {
            StreamError::Configuration("Pixel buffer is not backed by an IOSurface".into())
        })?;
        let iosurface_id = unsafe { IOSurfaceGetID(iosurface) };

        // Check if already checked in
        {
            let checked_in = self.inner.checked_in.lock();
            if let Some(existing_id) = checked_in.get_surface_id(iosurface_id) {
                tracing::trace!(
                    "SurfaceStore: Reusing existing surface ID '{}' for IOSurface {}",
                    existing_id,
                    iosurface_id
                );
                return Ok(existing_id.clone());
            }
        }

        // Export mach port from the pixel buffer
        let (_, mach_port) = pixel_buffer_ref.export_handle_as_mach_port()?;

        // Send to broker via XPC
        let surface_id = self.check_in_to_broker(mach_port)?;

        // Store reverse mapping
        self.inner
            .checked_in
            .lock()
            .insert(iosurface_id, surface_id.clone());

        // Also cache locally for fast checkout
        self.inner
            .cache
            .lock()
            .insert(surface_id.clone(), pixel_buffer.clone());

        tracing::debug!(
            "SurfaceStore: Checked in IOSurface {} as '{}'",
            iosurface_id,
            surface_id
        );

        Ok(surface_id)
    }

    /// Check out a surface by ID, returning the pixel buffer.
    ///
    /// Returns from cache if available, otherwise fetches from broker.
    #[cfg(target_os = "macos")]
    pub fn check_out(&self, surface_id: &str) -> Result<RhiPixelBuffer> {
        // Check cache first
        {
            let mut cache = self.inner.cache.lock();
            if let Some(cached) = cache.surfaces.get_mut(surface_id) {
                cached.checkout_count += 1;
                tracing::trace!(
                    "SurfaceStore: Cache hit for '{}' (checkout #{})",
                    surface_id,
                    cached.checkout_count
                );
                return Ok(cached.pixel_buffer.clone());
            }
        }

        // Cache miss - fetch from broker
        tracing::debug!(
            "SurfaceStore: Cache miss for '{}', fetching from broker",
            surface_id
        );
        let mach_port = self.check_out_from_broker(surface_id)?;

        // Import the pixel buffer from mach port
        use crate::core::rhi::{
            PixelFormat, RhiExternalHandle, RhiPixelBufferImport, RhiPixelBufferRef,
        };

        let handle = RhiExternalHandle::IOSurfaceMachPort { port: mach_port };
        // Width/height/format are extracted from the IOSurface itself after import
        // We pass dummy values as the import will query the actual values from the IOSurface
        let pixel_buffer_ref =
            RhiPixelBufferRef::from_external_handle(handle, 0, 0, PixelFormat::default())?;
        let pixel_buffer = RhiPixelBuffer::new(pixel_buffer_ref);

        // Cache for future use
        self.inner
            .cache
            .lock()
            .insert(surface_id.to_string(), pixel_buffer.clone());

        Ok(pixel_buffer)
    }

    /// Send check-in request to broker via XPC.
    #[cfg(target_os = "macos")]
    fn check_in_to_broker(&self, mach_port: u32) -> Result<String> {
        let connection = self.inner.connection.lock();
        let connection = connection.as_ref().ok_or_else(|| {
            StreamError::Configuration("SurfaceStore not connected to broker".into())
        })?;

        // Create request dictionary
        let request = unsafe { xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0) };
        if request.is_null() {
            return Err(StreamError::Configuration(
                "Failed to create XPC request dictionary".into(),
            ));
        }

        // Set operation type
        let op_key = CString::new("op").unwrap();
        let op_value = CString::new("check_in").unwrap();
        unsafe {
            xpc_dictionary_set_string(request, op_key.as_ptr(), op_value.as_ptr());
        }

        // Set runtime ID
        let runtime_id_key = CString::new("runtime_id").unwrap();
        let runtime_id_value = CString::new(self.inner.runtime_id.as_str()).unwrap();
        unsafe {
            xpc_dictionary_set_string(request, runtime_id_key.as_ptr(), runtime_id_value.as_ptr());
        }

        // Set mach port
        let port_key = CString::new("mach_port").unwrap();
        unsafe {
            xpc_dictionary_set_mach_send(request, port_key.as_ptr(), mach_port);
        }

        // Send and wait for reply
        let reply = unsafe { xpc_connection_send_message_with_reply_sync(*connection, request) };

        // Release request
        unsafe {
            xpc_release(request);
        }

        if reply.is_null() {
            return Err(StreamError::Configuration(
                "XPC check_in: null reply from broker".into(),
            ));
        }

        // Check for error
        if xpc_is_error(reply) {
            unsafe {
                xpc_release(reply);
            }
            return Err(StreamError::Configuration(
                "XPC check_in: broker returned error".into(),
            ));
        }

        // Extract surface_id from reply
        let surface_id_key = CString::new("surface_id").unwrap();
        let surface_id_ptr = unsafe { xpc_dictionary_get_string(reply, surface_id_key.as_ptr()) };

        if surface_id_ptr.is_null() {
            unsafe {
                xpc_release(reply);
            }
            return Err(StreamError::Configuration(
                "XPC check_in: missing surface_id in reply".into(),
            ));
        }

        let surface_id = unsafe { std::ffi::CStr::from_ptr(surface_id_ptr) }
            .to_string_lossy()
            .into_owned();

        unsafe {
            xpc_release(reply);
        }

        Ok(surface_id)
    }

    /// Send check-out request to broker via XPC.
    #[cfg(target_os = "macos")]
    fn check_out_from_broker(&self, surface_id: &str) -> Result<u32> {
        let connection = self.inner.connection.lock();
        let connection = connection.as_ref().ok_or_else(|| {
            StreamError::Configuration("SurfaceStore not connected to broker".into())
        })?;

        // Create request dictionary
        let request = unsafe { xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0) };
        if request.is_null() {
            return Err(StreamError::Configuration(
                "Failed to create XPC request dictionary".into(),
            ));
        }

        // Set operation type
        let op_key = CString::new("op").unwrap();
        let op_value = CString::new("check_out").unwrap();
        unsafe {
            xpc_dictionary_set_string(request, op_key.as_ptr(), op_value.as_ptr());
        }

        // Set surface ID
        let surface_id_key = CString::new("surface_id").unwrap();
        let surface_id_value = CString::new(surface_id).unwrap();
        unsafe {
            xpc_dictionary_set_string(request, surface_id_key.as_ptr(), surface_id_value.as_ptr());
        }

        // Send and wait for reply
        let reply = unsafe { xpc_connection_send_message_with_reply_sync(*connection, request) };

        // Release request
        unsafe {
            xpc_release(request);
        }

        if reply.is_null() {
            return Err(StreamError::Configuration(
                "XPC check_out: null reply from broker".into(),
            ));
        }

        // Check for error
        if xpc_is_error(reply) {
            unsafe {
                xpc_release(reply);
            }
            return Err(StreamError::Configuration(format!(
                "XPC check_out: broker returned error for surface '{}'",
                surface_id
            )));
        }

        // Extract mach_port from reply
        let port_key = CString::new("mach_port").unwrap();
        let mach_port = unsafe { xpc_dictionary_get_mach_send(reply, port_key.as_ptr()) };

        unsafe {
            xpc_release(reply);
        }

        if mach_port == 0 {
            return Err(StreamError::Configuration(format!(
                "XPC check_out: invalid mach port for surface '{}'",
                surface_id
            )));
        }

        Ok(mach_port)
    }

    /// Send release request to broker via XPC.
    #[cfg(target_os = "macos")]
    fn release_from_broker(&self, surface_id: &str) -> Result<()> {
        let connection = self.inner.connection.lock();
        let connection = connection.as_ref().ok_or_else(|| {
            StreamError::Configuration("SurfaceStore not connected to broker".into())
        })?;

        // Create request dictionary
        let request = unsafe { xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0) };
        if request.is_null() {
            return Err(StreamError::Configuration(
                "Failed to create XPC request dictionary".into(),
            ));
        }

        // Set operation type
        let op_key = CString::new("op").unwrap();
        let op_value = CString::new("release").unwrap();
        unsafe {
            xpc_dictionary_set_string(request, op_key.as_ptr(), op_value.as_ptr());
        }

        // Set surface ID
        let surface_id_key = CString::new("surface_id").unwrap();
        let surface_id_value = CString::new(surface_id).unwrap();
        unsafe {
            xpc_dictionary_set_string(request, surface_id_key.as_ptr(), surface_id_value.as_ptr());
        }

        // Set runtime ID
        let runtime_id_key = CString::new("runtime_id").unwrap();
        let runtime_id_value = CString::new(self.inner.runtime_id.as_str()).unwrap();
        unsafe {
            xpc_dictionary_set_string(request, runtime_id_key.as_ptr(), runtime_id_value.as_ptr());
        }

        // Send without waiting for reply (fire and forget for cleanup)
        unsafe {
            xpc_connection_send_message(*connection, request);
            xpc_release(request);
        }

        Ok(())
    }

    // =========================================================================
    // Non-macOS stubs
    // =========================================================================

    #[cfg(not(target_os = "macos"))]
    pub fn connect(&self) -> Result<()> {
        Err(StreamError::NotSupported(
            "SurfaceStore is only supported on macOS".into(),
        ))
    }

    #[cfg(not(target_os = "macos"))]
    pub fn disconnect(&self) -> Result<()> {
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    pub fn check_in(&self, _pixel_buffer: &RhiPixelBuffer) -> Result<String> {
        Err(StreamError::NotSupported(
            "SurfaceStore is only supported on macOS".into(),
        ))
    }

    #[cfg(not(target_os = "macos"))]
    pub fn check_out(&self, _surface_id: &str) -> Result<RhiPixelBuffer> {
        Err(StreamError::NotSupported(
            "SurfaceStore is only supported on macOS".into(),
        ))
    }
}

// Safety: XPC connections are thread-safe
unsafe impl Send for SurfaceStoreInner {}
unsafe impl Sync for SurfaceStoreInner {}

impl std::fmt::Debug for SurfaceStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SurfaceStore")
            .field("service_name", &self.inner.service_name)
            .field("runtime_id", &self.inner.runtime_id)
            .finish()
    }
}
