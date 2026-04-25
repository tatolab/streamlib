// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Surface Store for cross-process GPU surface sharing.
//!
//! Provides check-in/check-out semantics for IOSurfaces via the macOS XPC surface-share service.
//! Surfaces are cached locally after first checkout to minimize XPC overhead.

use std::collections::HashMap;
#[cfg(target_os = "macos")]
use std::ffi::CString;
use std::sync::Arc;

#[cfg(target_os = "macos")]
use std::ffi::c_void;

use parking_lot::Mutex;

use crate::core::rhi::RhiPixelBuffer;
use crate::core::{Result, StreamError};

/// Maximum number of entries in the SurfaceCache before eviction.
const MAX_SURFACE_CACHE_SIZE: usize = 512;

#[cfg(target_os = "macos")]
use crate::apple::xpc_ffi::{
    _NSConcreteMallocBlock, xpc_connection_cancel, xpc_connection_create_mach_service,
    xpc_connection_resume, xpc_connection_send_message,
    xpc_connection_send_message_with_reply_sync, xpc_connection_set_event_handler,
    xpc_connection_t, xpc_dictionary_copy_mach_send, xpc_dictionary_create,
    xpc_dictionary_get_string, xpc_dictionary_set_mach_send, xpc_dictionary_set_string,
    xpc_error_connection_interrupted, xpc_error_connection_invalid, xpc_is_error, xpc_object_t,
    xpc_release, Block, BlockDescriptor, BLOCK_FLAGS_NEEDS_FREE,
};

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

    fn insert(&mut self, surface_id: String, pixel_buffer: RhiPixelBuffer) {
        self.surfaces.insert(
            surface_id,
            CachedSurface {
                pixel_buffer,
                checkout_count: 1,
            },
        );
        if self.surfaces.len() > MAX_SURFACE_CACHE_SIZE {
            tracing::warn!(
                "SurfaceCache: exceeded {} entries ({}), clearing",
                MAX_SURFACE_CACHE_SIZE,
                self.surfaces.len()
            );
            self.surfaces.clear();
        }
    }

    fn clear(&mut self) {
        self.surfaces.clear();
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
/// Connects to the macOS XPC surface-share service to exchange mach ports for surface IDs.
/// Caches resolved surfaces locally to minimize XPC round-trips.
#[derive(Clone)]
pub struct SurfaceStore {
    inner: Arc<SurfaceStoreInner>,
}

struct SurfaceStoreInner {
    /// XPC connection to the surface-share service (macOS only).
    #[cfg(target_os = "macos")]
    connection: Mutex<Option<xpc_connection_t>>,

    /// Unix socket connection to the surface-share service (Linux only).
    #[cfg(target_os = "linux")]
    connection: Mutex<Option<std::os::unix::net::UnixStream>>,

    /// Local cache of checked-out surfaces (surface_id -> pixel_buffer).
    cache: Mutex<SurfaceCache>,

    /// Reverse lookup for checked-in surfaces (iosurface_id -> surface_id).
    checked_in: Mutex<CheckedInSurfaces>,

    /// The XPC service name (macOS) or Unix socket path (Linux) to connect to.
    service_name: String,

    /// Runtime ID for tracking which surfaces belong to this runtime.
    runtime_id: String,
}

impl SurfaceStore {
    /// Create a new surface store (not yet connected).
    pub fn new(service_name: String, runtime_id: String) -> Self {
        Self {
            inner: Arc::new(SurfaceStoreInner {
                #[cfg(any(target_os = "macos", target_os = "linux"))]
                connection: Mutex::new(None),
                cache: Mutex::new(SurfaceCache::new()),
                checked_in: Mutex::new(CheckedInSurfaces::new()),
                service_name,
                runtime_id,
            }),
        }
    }

    /// Connect to the macOS XPC surface-share service.
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
        // We use synchronous send/reply, so the handler just logs connection errors
        unsafe {
            let handler = create_xpc_event_handler();
            xpc_connection_set_event_handler(connection, handler);
            xpc_connection_resume(connection);
        }

        *self.inner.connection.lock() = Some(connection);

        tracing::info!(
            "SurfaceStore: Connected to XPC service '{}'",
            self.inner.service_name
        );

        Ok(())
    }

    /// Disconnect from the surface-share service and release all surfaces.
    ///
    /// This should be called during runtime.stop().
    #[cfg(target_os = "macos")]
    pub fn disconnect(&self) -> Result<()> {
        // Release all checked-in surfaces from the surface-share service
        let surface_ids = self.inner.checked_in.lock().surface_ids();
        for surface_id in surface_ids {
            if let Err(e) = self.release_from_surface_share(&surface_id) {
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
    /// Otherwise, sends the mach port to the surface-share service and receives a new ID.
    #[cfg(target_os = "macos")]
    pub fn check_in(&self, pixel_buffer: &RhiPixelBuffer) -> Result<String> {
        use crate::apple::corevideo_ffi::{mach_port_deallocate, mach_task_self, IOSurfaceGetID};

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

        // Send to surface-share service via XPC
        let surface_id = self.check_in_to_surface_share(mach_port);

        // Deallocate our copy of the mach port - XPC copied the send right to its dictionary,
        // so we must release ours to avoid leaking ports
        let task = unsafe { mach_task_self() };
        let dealloc_result = unsafe { mach_port_deallocate(task, mach_port) };
        if dealloc_result != 0 {
            tracing::warn!(
                "SurfaceStore: Failed to deallocate mach_port={}: error {}",
                mach_port,
                dealloc_result
            );
        }

        // Now propagate any error from the surface-share service call
        let surface_id = surface_id?;

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
    /// Returns from cache if available, otherwise fetches from the surface-share service.
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

        // Cache miss - fetch from the surface-share service
        tracing::debug!(
            "SurfaceStore: Cache miss for '{}', fetching from the surface-share service",
            surface_id
        );
        let mach_port = self.check_out_from_surface_share(surface_id)?;

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

    /// Send check-in request to surface-share service via XPC.
    #[cfg(target_os = "macos")]
    fn check_in_to_surface_share(&self, mach_port: u32) -> Result<String> {
        let connection = self.inner.connection.lock();
        let connection = connection.as_ref().ok_or_else(|| {
            StreamError::Configuration("SurfaceStore not connected to surface-share service".into())
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
                "XPC check_in: null reply from the surface-share service".into(),
            ));
        }

        // Check for error
        if xpc_is_error(reply) {
            unsafe {
                xpc_release(reply);
            }
            return Err(StreamError::Configuration(
                "XPC check_in: surface-share service returned error".into(),
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

    /// Send check-out request to surface-share service via XPC.
    #[cfg(target_os = "macos")]
    fn check_out_from_surface_share(&self, surface_id: &str) -> Result<u32> {
        let connection = self.inner.connection.lock();
        let connection = connection.as_ref().ok_or_else(|| {
            StreamError::Configuration("SurfaceStore not connected to surface-share service".into())
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
                "XPC check_out: null reply from the surface-share service".into(),
            ));
        }

        // Check for error
        if xpc_is_error(reply) {
            unsafe {
                xpc_release(reply);
            }
            return Err(StreamError::Configuration(format!(
                "XPC check_out: surface-share service returned error for surface '{}'",
                surface_id
            )));
        }

        // Extract mach_port from reply
        let port_key = CString::new("mach_port").unwrap();
        let mach_port = unsafe { xpc_dictionary_copy_mach_send(reply, port_key.as_ptr()) };

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

    /// Register a buffer with the surface-share service using the new protocol.
    ///
    /// The client provides the UUID (PixelBufferPoolId) and the buffer.
    /// This is used for pre-registering pooled buffers.
    #[cfg(target_os = "macos")]
    pub fn register_buffer(&self, pool_id: &str, pixel_buffer: &RhiPixelBuffer) -> Result<()> {
        use crate::apple::corevideo_ffi::{mach_port_deallocate, mach_task_self};

        // Export mach port from the pixel buffer
        let pixel_buffer_ref = pixel_buffer.buffer_ref();
        let (_, mach_port) = pixel_buffer_ref.export_handle_as_mach_port()?;

        // Register with the surface-share service
        let result = self.register_with_surface_share(pool_id, mach_port);

        // Deallocate our copy of the mach port
        let task = unsafe { mach_task_self() };
        let dealloc_result = unsafe { mach_port_deallocate(task, mach_port) };
        if dealloc_result != 0 {
            tracing::warn!(
                "SurfaceStore: Failed to deallocate mach_port={}: error {}",
                mach_port,
                dealloc_result
            );
        }

        result
    }

    /// Send register request to surface-share service via XPC (new protocol).
    #[cfg(target_os = "macos")]
    fn register_with_surface_share(&self, pool_id: &str, mach_port: u32) -> Result<()> {
        let connection = self.inner.connection.lock();
        let connection = connection.as_ref().ok_or_else(|| {
            StreamError::Configuration("SurfaceStore not connected to surface-share service".into())
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
        let op_value = CString::new("register").unwrap();
        unsafe {
            xpc_dictionary_set_string(request, op_key.as_ptr(), op_value.as_ptr());
        }

        // Set surface_id (the UUID we're providing)
        let surface_id_key = CString::new("surface_id").unwrap();
        let surface_id_value = CString::new(pool_id).unwrap();
        unsafe {
            xpc_dictionary_set_string(request, surface_id_key.as_ptr(), surface_id_value.as_ptr());
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
                "XPC register: null reply from the surface-share service".into(),
            ));
        }

        // Check for error
        if xpc_is_error(reply) {
            unsafe {
                xpc_release(reply);
            }
            return Err(StreamError::Configuration(
                "XPC register: surface-share service returned error".into(),
            ));
        }

        // Check for error message in reply
        let error_key = CString::new("error").unwrap();
        let error_ptr = unsafe { xpc_dictionary_get_string(reply, error_key.as_ptr()) };
        if !error_ptr.is_null() {
            let error_msg = unsafe { std::ffi::CStr::from_ptr(error_ptr) }
                .to_string_lossy()
                .into_owned();
            unsafe {
                xpc_release(reply);
            }
            return Err(StreamError::Configuration(format!(
                "XPC register: {}",
                error_msg
            )));
        }

        unsafe {
            xpc_release(reply);
        }

        tracing::debug!("SurfaceStore: Registered buffer '{}'", pool_id);
        Ok(())
    }

    /// Lookup a buffer from the surface-share service using the new protocol.
    ///
    /// Returns the mach port for the given UUID.
    #[cfg(target_os = "macos")]
    pub fn lookup_buffer(&self, pool_id: &str) -> Result<RhiPixelBuffer> {
        let mach_port = self.lookup_from_surface_share(pool_id)?;

        // Import the pixel buffer from mach port
        use crate::core::rhi::{
            PixelFormat, RhiExternalHandle, RhiPixelBufferImport, RhiPixelBufferRef,
        };

        let handle = RhiExternalHandle::IOSurfaceMachPort { port: mach_port };
        let pixel_buffer_ref =
            RhiPixelBufferRef::from_external_handle(handle, 0, 0, PixelFormat::default())?;
        Ok(RhiPixelBuffer::new(pixel_buffer_ref))
    }

    /// Send lookup request to surface-share service via XPC (new protocol).
    #[cfg(target_os = "macos")]
    fn lookup_from_surface_share(&self, pool_id: &str) -> Result<u32> {
        let connection = self.inner.connection.lock();
        let connection = connection.as_ref().ok_or_else(|| {
            StreamError::Configuration("SurfaceStore not connected to surface-share service".into())
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
        let op_value = CString::new("lookup").unwrap();
        unsafe {
            xpc_dictionary_set_string(request, op_key.as_ptr(), op_value.as_ptr());
        }

        // Set surface_id (the UUID we're looking up)
        let surface_id_key = CString::new("surface_id").unwrap();
        let surface_id_value = CString::new(pool_id).unwrap();
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
                "XPC lookup: null reply from the surface-share service".into(),
            ));
        }

        // Check for error
        if xpc_is_error(reply) {
            unsafe {
                xpc_release(reply);
            }
            return Err(StreamError::Configuration(format!(
                "XPC lookup: surface-share service returned error for '{}'",
                pool_id
            )));
        }

        // Check for error message in reply
        let error_key = CString::new("error").unwrap();
        let error_ptr = unsafe { xpc_dictionary_get_string(reply, error_key.as_ptr()) };
        if !error_ptr.is_null() {
            let error_msg = unsafe { std::ffi::CStr::from_ptr(error_ptr) }
                .to_string_lossy()
                .into_owned();
            unsafe {
                xpc_release(reply);
            }
            return Err(StreamError::Configuration(format!(
                "XPC lookup: {}",
                error_msg
            )));
        }

        // Extract mach_port from reply
        let port_key = CString::new("mach_port").unwrap();
        let mach_port = unsafe { xpc_dictionary_copy_mach_send(reply, port_key.as_ptr()) };

        unsafe {
            xpc_release(reply);
        }

        if mach_port == 0 {
            return Err(StreamError::Configuration(format!(
                "XPC lookup: invalid mach port for '{}'",
                pool_id
            )));
        }

        Ok(mach_port)
    }

    /// Release a single surface from the surface-share service. Platform-dispatched.
    ///
    /// Fire-and-forget on macOS (mirrors `release_from_surface_share`). On Linux the
    /// surface-share service's `release` op is best-effort; a missing connection returns Ok
    /// since the surface-share service already treats the client's socket-close as a full
    /// release.
    pub fn release(&self, surface_id: &str) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            self.release_from_surface_share(surface_id)
        }
        #[cfg(target_os = "linux")]
        {
            self.release_from_surface_share_unix(surface_id)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = surface_id;
            Err(StreamError::NotSupported(
                "SurfaceStore::release is only supported on macOS and Linux".into(),
            ))
        }
    }

    /// Send release request to surface-share service via XPC.
    #[cfg(target_os = "macos")]
    fn release_from_surface_share(&self, surface_id: &str) -> Result<()> {
        let connection = self.inner.connection.lock();
        let connection = connection.as_ref().ok_or_else(|| {
            StreamError::Configuration("SurfaceStore not connected to surface-share service".into())
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
    // Linux: Unix socket client
    // =========================================================================

    /// Connect to the surface-share Unix socket.
    #[cfg(target_os = "linux")]
    pub fn connect(&self) -> Result<()> {
        let stream = std::os::unix::net::UnixStream::connect(&self.inner.service_name)
            .map_err(|e| {
                StreamError::Configuration(format!(
                    "Failed to connect to surface-share socket '{}': {}",
                    self.inner.service_name, e
                ))
            })?;

        *self.inner.connection.lock() = Some(stream);

        tracing::info!(
            "SurfaceStore: Connected to surface-share service socket '{}'",
            self.inner.service_name
        );

        Ok(())
    }

    /// Disconnect from the surface-share service and release all surfaces.
    #[cfg(target_os = "linux")]
    pub fn disconnect(&self) -> Result<()> {
        // Release all checked-in surfaces
        let surface_ids = self.inner.checked_in.lock().surface_ids();
        for surface_id in surface_ids {
            if let Err(e) = self.release_from_surface_share_unix(&surface_id) {
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

        // Drop the connection
        self.inner.connection.lock().take();

        tracing::info!("SurfaceStore: Disconnected from surface-share socket");
        Ok(())
    }

    /// Check in a pixel buffer via Unix socket, returning a surface ID.
    #[cfg(target_os = "linux")]
    pub fn check_in(&self, pixel_buffer: &RhiPixelBuffer) -> Result<String> {
        use crate::core::rhi::RhiPixelBufferExport;

        // Export every plane's fd. Single-plane pixel buffers return a
        // one-element vec; multi-plane DMA-BUFs (e.g. NV12 under DRM format
        // modifiers) return one fd per plane.
        let planes = pixel_buffer.export_plane_handles()?;
        let mut plane_fds: Vec<std::os::unix::io::RawFd> = Vec::with_capacity(planes.len());
        let mut plane_sizes: Vec<u64> = Vec::with_capacity(planes.len());
        let mut plane_offsets: Vec<u64> = Vec::with_capacity(planes.len());
        for handle in planes {
            let crate::core::rhi::RhiExternalHandle::DmaBuf { fd, size } = handle;
            plane_fds.push(fd);
            plane_sizes.push(size as u64);
            plane_offsets.push(0);
        }

        let request = serde_json::json!({
            "op": "check_in",
            "runtime_id": self.inner.runtime_id,
            "width": pixel_buffer.width,
            "height": pixel_buffer.height,
            "format": format!("{:?}", pixel_buffer.format()),
            "plane_sizes": plane_sizes,
            "plane_offsets": plane_offsets,
        });

        let connection = self.inner.connection.lock();
        let stream = connection.as_ref().ok_or_else(|| {
            StreamError::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        let send_result =
            streamlib_surface_client::send_request_with_fds(stream, &request, &plane_fds, 0);

        // Close the exported fds (surface-share service has its own dups) regardless of
        // the request outcome — the peer owns its kernel-delivered fds and
        // we never keep ours.
        for fd in &plane_fds {
            unsafe { libc::close(*fd) };
        }

        let (response, response_fds) = send_result.map_err(|e| {
            StreamError::Configuration(format!("Unix socket check_in failed: {}", e))
        })?;
        // check_in never returns fds; close any the surface-share service may have attached
        // defensively so a future protocol drift doesn't leak them.
        for fd in &response_fds {
            unsafe { libc::close(*fd) };
        }

        let surface_id = response
            .get("surface_id")
            .and_then(|v: &serde_json::Value| v.as_str())
            .ok_or_else(|| {
                StreamError::Configuration("check_in: missing surface_id in response".into())
            })?
            .to_string();

        self.inner
            .cache
            .lock()
            .insert(surface_id.clone(), pixel_buffer.clone());

        tracing::debug!("SurfaceStore: Checked in as '{}'", surface_id);

        Ok(surface_id)
    }

    /// Check out a surface by ID via Unix socket.
    #[cfg(target_os = "linux")]
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

        // Cache miss - fetch from the surface-share service
        tracing::debug!(
            "SurfaceStore: Cache miss for '{}', fetching from the surface-share service",
            surface_id
        );

        let request = serde_json::json!({
            "op": "check_out",
            "surface_id": surface_id,
        });

        let connection = self.inner.connection.lock();
        let stream = connection.as_ref().ok_or_else(|| {
            StreamError::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        let (response, received_fds) = streamlib_surface_client::send_request_with_fds(
            stream,
            &request,
            &[],
            streamlib_surface_client::MAX_DMA_BUF_PLANES,
        )
        .map_err(|e| {
            StreamError::Configuration(format!("Unix socket check_out failed: {}", e))
        })?;

        if let Some(error) = response.get("error").and_then(|v: &serde_json::Value| v.as_str()) {
            for fd in &received_fds {
                unsafe { libc::close(*fd) };
            }
            return Err(StreamError::Configuration(format!(
                "check_out: {}",
                error
            )));
        }

        if received_fds.is_empty() {
            return Err(StreamError::Configuration(
                "check_out: no DMA-BUF fd in response".into(),
            ));
        }

        // Import every plane as a `RhiExternalHandle::DmaBuf`. The Rust
        // importer now tracks the full vec symmetrically with the
        // polyglot Python / Deno shims — no plane is dropped.
        use crate::core::rhi::{PixelFormat, RhiExternalHandle, RhiPixelBufferImport};

        let plane_sizes: Vec<u64> = response
            .get("plane_sizes")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_u64()).collect())
            .filter(|v: &Vec<u64>| v.len() == received_fds.len())
            .unwrap_or_else(|| vec![0u64; received_fds.len()]);

        let handles: Vec<RhiExternalHandle> = received_fds
            .iter()
            .zip(plane_sizes.iter())
            .map(|(fd, size)| RhiExternalHandle::DmaBuf {
                fd: *fd,
                size: *size as usize,
            })
            .collect();
        let pixel_buffer =
            RhiPixelBuffer::from_external_plane_handles(&handles, 0, 0, PixelFormat::default())?;

        // Cache for future use
        self.inner
            .cache
            .lock()
            .insert(surface_id.to_string(), pixel_buffer.clone());

        Ok(pixel_buffer)
    }

    /// Register a buffer with the surface-share service via Unix socket.
    #[cfg(target_os = "linux")]
    pub fn register_buffer(&self, pool_id: &str, pixel_buffer: &RhiPixelBuffer) -> Result<()> {
        use crate::core::rhi::RhiPixelBufferExport;

        // Export the DMA-BUF fd
        let handle = pixel_buffer.export_handle()?;
        let (fd, _size) = match handle {
            crate::core::rhi::RhiExternalHandle::DmaBuf { fd, size } => (fd, size),
        };

        let request = serde_json::json!({
            "op": "register",
            "surface_id": pool_id,
            "runtime_id": self.inner.runtime_id,
            "width": pixel_buffer.width,
            "height": pixel_buffer.height,
            "format": format!("{:?}", pixel_buffer.format()),
            "resource_type": "pixel_buffer",
        });

        let connection = self.inner.connection.lock();
        let stream = connection.as_ref().ok_or_else(|| {
            StreamError::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        let send_result =
            streamlib_surface_client::send_request_with_fds(stream, &request, &[fd], 0);
        unsafe { libc::close(fd) };
        let (response, response_fds) = send_result.map_err(|e| {
            StreamError::Configuration(format!("Unix socket register failed: {}", e))
        })?;
        for f in &response_fds {
            unsafe { libc::close(*f) };
        }

        if let Some(error) = response.get("error").and_then(|v: &serde_json::Value| v.as_str()) {
            return Err(StreamError::Configuration(format!(
                "register: {}",
                error
            )));
        }

        tracing::debug!("SurfaceStore: Registered buffer '{}'", pool_id);
        Ok(())
    }

    /// Register a texture with the surface-share service via Unix socket.
    #[cfg(target_os = "linux")]
    pub fn register_texture(
        &self,
        surface_id: &str,
        texture: &crate::core::rhi::StreamTexture,
    ) -> Result<()> {
        // Export the DMA-BUF fd from the texture
        let fd = texture.inner.export_dma_buf_fd()?;

        let request = serde_json::json!({
            "op": "register",
            "surface_id": surface_id,
            "runtime_id": self.inner.runtime_id,
            "width": texture.width(),
            "height": texture.height(),
            "format": format!("{:?}", texture.format()),
            "resource_type": "texture",
        });

        let connection = self.inner.connection.lock();
        let stream = connection.as_ref().ok_or_else(|| {
            StreamError::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        let send_result =
            streamlib_surface_client::send_request_with_fds(stream, &request, &[fd], 0);
        unsafe { libc::close(fd) };
        let (response, response_fds) = send_result.map_err(|e| {
            StreamError::Configuration(format!("Unix socket register_texture failed: {}", e))
        })?;
        for f in &response_fds {
            unsafe { libc::close(*f) };
        }

        if let Some(error) = response.get("error").and_then(|v: &serde_json::Value| v.as_str()) {
            return Err(StreamError::Configuration(format!(
                "register_texture: {}",
                error
            )));
        }

        tracing::debug!("SurfaceStore: Registered texture '{}'", surface_id);
        Ok(())
    }

    /// Lookup a buffer from the surface-share service via Unix socket.
    #[cfg(target_os = "linux")]
    pub fn lookup_buffer(&self, pool_id: &str) -> Result<RhiPixelBuffer> {
        let request = serde_json::json!({
            "op": "lookup",
            "surface_id": pool_id,
        });

        let connection = self.inner.connection.lock();
        let stream = connection.as_ref().ok_or_else(|| {
            StreamError::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        let (response, received_fds) = streamlib_surface_client::send_request_with_fds(
            stream,
            &request,
            &[],
            streamlib_surface_client::MAX_DMA_BUF_PLANES,
        )
        .map_err(|e| {
            StreamError::Configuration(format!("Unix socket lookup failed: {}", e))
        })?;

        if let Some(error) = response.get("error").and_then(|v: &serde_json::Value| v.as_str()) {
            for fd in &received_fds {
                unsafe { libc::close(*fd) };
            }
            return Err(StreamError::Configuration(format!(
                "lookup: {}",
                error
            )));
        }

        if received_fds.is_empty() {
            return Err(StreamError::Configuration(
                "lookup: no DMA-BUF fd in response".into(),
            ));
        }

        use crate::core::rhi::{PixelFormat, RhiExternalHandle, RhiPixelBufferImport};

        let plane_sizes: Vec<u64> = response
            .get("plane_sizes")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_u64()).collect())
            .filter(|v: &Vec<u64>| v.len() == received_fds.len())
            .unwrap_or_else(|| vec![0u64; received_fds.len()]);

        let handles: Vec<RhiExternalHandle> = received_fds
            .iter()
            .zip(plane_sizes.iter())
            .map(|(fd, size)| RhiExternalHandle::DmaBuf {
                fd: *fd,
                size: *size as usize,
            })
            .collect();
        RhiPixelBuffer::from_external_plane_handles(&handles, 0, 0, PixelFormat::default())
    }

    /// Lookup a texture from the surface-share service via Unix socket.
    #[cfg(target_os = "linux")]
    pub fn lookup_texture(&self, surface_id: &str) -> Result<crate::core::rhi::StreamTexture> {
        let request = serde_json::json!({
            "op": "lookup",
            "surface_id": surface_id,
        });

        let connection = self.inner.connection.lock();
        let stream = connection.as_ref().ok_or_else(|| {
            StreamError::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        let (response, received_fds) = streamlib_surface_client::send_request_with_fds(
            stream,
            &request,
            &[],
            streamlib_surface_client::MAX_DMA_BUF_PLANES,
        )
        .map_err(|e| {
            StreamError::Configuration(format!("Unix socket lookup_texture failed: {}", e))
        })?;

        if let Some(error) = response.get("error").and_then(|v: &serde_json::Value| v.as_str()) {
            for fd in &received_fds {
                unsafe { libc::close(*fd) };
            }
            return Err(StreamError::Configuration(format!(
                "lookup_texture: {}",
                error
            )));
        }

        if received_fds.is_empty() {
            return Err(StreamError::Configuration(
                "lookup_texture: no DMA-BUF fd in response".into(),
            ));
        }
        let dma_buf_fd = received_fds[0];
        for fd in &received_fds[1..] {
            unsafe { libc::close(*fd) };
        }

        // Extract width, height, format from the response
        let width = response
            .get("width")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                StreamError::Configuration("lookup_texture: missing width in response".into())
            })? as u32;

        let height = response
            .get("height")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                StreamError::Configuration("lookup_texture: missing height in response".into())
            })? as u32;

        let format_str = response
            .get("format")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                StreamError::Configuration("lookup_texture: missing format in response".into())
            })?;

        use crate::core::rhi::TextureFormat;

        let format = match format_str {
            "Rgba8Unorm" => TextureFormat::Rgba8Unorm,
            "Rgba8UnormSrgb" => TextureFormat::Rgba8UnormSrgb,
            "Bgra8Unorm" => TextureFormat::Bgra8Unorm,
            "Bgra8UnormSrgb" => TextureFormat::Bgra8UnormSrgb,
            "Rgba16Float" => TextureFormat::Rgba16Float,
            "Rgba32Float" => TextureFormat::Rgba32Float,
            "Nv12" => TextureFormat::Nv12,
            _ => {
                return Err(StreamError::Configuration(format!(
                    "lookup_texture: unknown format '{}'",
                    format_str
                )));
            }
        };

        let allocation_size = (width as u64) * (height as u64) * (format.bytes_per_pixel() as u64);

        let vulkan_device =
            crate::vulkan::rhi::vulkan_pixel_buffer::VULKAN_DEVICE_FOR_IMPORT
                .get()
                .ok_or_else(|| {
                    StreamError::NotSupported(
                        "lookup_texture: VulkanDevice not initialized for import".into(),
                    )
                })?;

        let vulkan_texture = crate::vulkan::rhi::VulkanTexture::from_dma_buf_fd(
            vulkan_device,
            dma_buf_fd,
            width,
            height,
            format,
            allocation_size,
        )?;

        Ok(crate::core::rhi::StreamTexture::from_vulkan(vulkan_texture))
    }

    /// Send release request to surface-share service via Unix socket.
    #[cfg(target_os = "linux")]
    fn release_from_surface_share_unix(&self, surface_id: &str) -> Result<()> {
        let request = serde_json::json!({
            "op": "release",
            "surface_id": surface_id,
            "runtime_id": self.inner.runtime_id,
        });

        let connection = self.inner.connection.lock();
        let stream = match connection.as_ref() {
            Some(s) => s,
            None => return Ok(()), // Already disconnected
        };

        let _ = streamlib_surface_client::send_request_with_fds(stream, &request, &[], 0);
        Ok(())
    }

    // =========================================================================
    // Unsupported platform stubs
    // =========================================================================

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn connect(&self) -> Result<()> {
        Err(StreamError::NotSupported(
            "SurfaceStore is only supported on macOS and Linux".into(),
        ))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn disconnect(&self) -> Result<()> {
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn check_in(&self, _pixel_buffer: &RhiPixelBuffer) -> Result<String> {
        Err(StreamError::NotSupported(
            "SurfaceStore is only supported on macOS and Linux".into(),
        ))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn check_out(&self, _surface_id: &str) -> Result<RhiPixelBuffer> {
        Err(StreamError::NotSupported(
            "SurfaceStore is only supported on macOS and Linux".into(),
        ))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn register_buffer(&self, _pool_id: &str, _pixel_buffer: &RhiPixelBuffer) -> Result<()> {
        Err(StreamError::NotSupported(
            "SurfaceStore is only supported on macOS and Linux".into(),
        ))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn lookup_buffer(&self, _pool_id: &str) -> Result<RhiPixelBuffer> {
        Err(StreamError::NotSupported(
            "SurfaceStore is only supported on macOS and Linux".into(),
        ))
    }

    #[cfg(not(target_os = "linux"))]
    pub fn register_texture(
        &self,
        _surface_id: &str,
        _texture: &crate::core::rhi::StreamTexture,
    ) -> Result<()> {
        Err(StreamError::NotSupported(
            "Texture registration not supported on this platform".into(),
        ))
    }

    #[cfg(not(target_os = "linux"))]
    pub fn lookup_texture(
        &self,
        _surface_id: &str,
    ) -> Result<crate::core::rhi::StreamTexture> {
        Err(StreamError::NotSupported(
            "Texture lookup not supported on this platform".into(),
        ))
    }
}

// Safety: XPC connections are thread-safe
unsafe impl Send for SurfaceStoreInner {}
unsafe impl Sync for SurfaceStoreInner {}

// =============================================================================
// XPC Block Helper (macOS only)
// =============================================================================

/// Create a minimal XPC event handler block for client connections.
///
/// This handler logs connection errors but otherwise does nothing, since we use
/// synchronous send/reply calls.
#[cfg(target_os = "macos")]
unsafe fn create_xpc_event_handler() -> *mut c_void {
    // Trampoline function that handles XPC events
    extern "C" fn event_handler_trampoline(_block: *mut Block<()>, event: xpc_object_t) {
        if xpc_is_error(event) {
            if event == xpc_error_connection_invalid() {
                tracing::debug!("SurfaceStore: XPC connection invalid");
            } else if event == xpc_error_connection_interrupted() {
                tracing::debug!("SurfaceStore: XPC connection interrupted");
            }
        }
    }

    // Block descriptor (static, no copy/dispose needed for this simple case)
    static DESCRIPTOR: BlockDescriptor = BlockDescriptor {
        reserved: 0,
        size: std::mem::size_of::<Block<()>>() as u64,
    };

    // Create heap-allocated block with proper ABI
    let block = Box::new(Block {
        isa: &_NSConcreteMallocBlock as *const _,
        flags: BLOCK_FLAGS_NEEDS_FREE,
        reserved: 0,
        invoke: event_handler_trampoline as *const c_void,
        descriptor: &DESCRIPTOR,
        context: (),
    });

    Box::into_raw(block) as *mut c_void
}

impl std::fmt::Debug for SurfaceStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SurfaceStore")
            .field("service_name", &self.inner.service_name)
            .field("runtime_id", &self.inner.runtime_id)
            .finish()
    }
}
