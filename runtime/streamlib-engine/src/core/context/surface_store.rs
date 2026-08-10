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

#[cfg(target_os = "linux")]
use std::os::fd::FromRawFd as _;

use crate::core::rhi::PixelBuffer;
#[cfg(target_os = "linux")]
use crate::core::rhi::PixelFormat;
use crate::core::{Error, Result};
#[cfg(target_os = "linux")]
use crate::host_rhi::HostTextureExt;

/// Every plane of a pixel buffer exported for the surface-share wire: the
/// fds to attach, the metadata arrays describing them, and the one
/// handle-type discriminator the checkout importer dispatches on.
#[cfg(target_os = "linux")]
struct ExportedPlaneWireHandles {
    plane_fds: Vec<std::os::unix::io::RawFd>,
    plane_sizes: Vec<u64>,
    plane_offsets: Vec<u64>,
    handle_type: &'static str,
}

/// Export every plane of `pixel_buffer` for a surface-share registration.
///
/// Single-plane pixel buffers return a one-element vec; multi-plane
/// DMA-BUFs (e.g. NV12 under DRM format modifiers) one fd per plane;
/// OPAQUE_FD-flavored buffers (CUDA targets) a single OPAQUE_FD handle.
/// One registration carries one flavour — a buffer whose planes export
/// under two is refused here rather than published under whichever
/// flavour happened to come last, which would import the other planes
/// through the wrong Vulkan external-handle type.
#[cfg(target_os = "linux")]
fn exported_plane_wire_handles(pixel_buffer: &PixelBuffer) -> Result<ExportedPlaneWireHandles> {
    use crate::core::rhi::{RhiExternalHandle, RhiPixelBufferExport};

    let planes = pixel_buffer.export_plane_handles()?;
    let mut plane_fds: Vec<std::os::unix::io::RawFd> = Vec::with_capacity(planes.len());
    let mut plane_sizes: Vec<u64> = Vec::with_capacity(planes.len());
    let mut plane_offsets: Vec<u64> = Vec::with_capacity(planes.len());
    let mut plane_flavours: Vec<&'static str> = Vec::with_capacity(planes.len());
    for handle in planes {
        let (fd, size, this_plane_flavour) = match handle {
            RhiExternalHandle::DmaBuf { fd, size } => (fd, size, "dma_buf"),
            RhiExternalHandle::OpaqueFd { fd, size } => (fd, size, "opaque_fd"),
        };
        plane_fds.push(fd);
        plane_sizes.push(size as u64);
        plane_offsets.push(0);
        plane_flavours.push(this_plane_flavour);
    }
    // Validated after every fd is collected, so the refusal closes all of
    // them rather than only the ones iterated so far.
    let handle_type = plane_flavours.first().copied().unwrap_or("dma_buf");
    if plane_flavours
        .iter()
        .any(|plane_flavour| *plane_flavour != handle_type)
    {
        // SAFETY: closing exported fds this process owns and has not handed
        // to anyone.
        unsafe {
            for collected_fd in &plane_fds {
                libc::close(*collected_fd);
            }
        }
        return Err(Error::Configuration(format!(
            "pixel buffer exports mixed external-handle flavours ({plane_flavours:?})"
        )));
    }
    Ok(ExportedPlaneWireHandles {
        plane_fds,
        plane_sizes,
        plane_offsets,
        handle_type,
    })
}

/// Adopt freshly-exported fds so they close exactly once, at the end of the
/// registration that sends them.
///
/// SAFETY contract of every caller: each fd came from an export that minted
/// it for this process and handed it to no one else.
#[cfg(target_os = "linux")]
fn adopt_exported_fds(exported_fds: Vec<std::os::unix::io::RawFd>) -> Vec<std::os::fd::OwnedFd> {
    exported_fds
        .into_iter()
        .map(|exported_fd| unsafe { std::os::fd::OwnedFd::from_raw_fd(exported_fd) })
        .collect()
}

/// Maximum number of entries in the SurfaceCache before eviction.
const MAX_SURFACE_CACHE_SIZE: usize = 512;

/// Wire value for a DMA-BUF-flavoured surface handle. The default: every
/// surface registered before the flavour was expressible carries no
/// `handle_type` at all.
#[cfg(target_os = "linux")]
const SURFACE_HANDLE_TYPE_DMA_BUF: &str = "dma_buf";

/// Wire value for an OPAQUE_FD-flavoured surface handle — what a Vulkan-aware
/// importer registers, including the device-export staging a helper process
/// reaches.
#[cfg(target_os = "linux")]
const SURFACE_HANDLE_TYPE_OPAQUE_FD: &str = "opaque_fd";

/// Wrap each received fd as the external-handle flavour the surface-share
/// service registered it under.
///
/// The two flavours are distinct Vulkan external-handle types. Importing an
/// OPAQUE_FD fd through the DMA-BUF type does not fail cleanly — it hands the
/// driver a handle of the wrong type — so the wire's discriminator is honoured
/// rather than assumed.
#[cfg(target_os = "linux")]
fn external_plane_handles_for_flavour(
    handle_type: &str,
    received_fds: &[std::os::fd::RawFd],
    plane_sizes: &[u64],
) -> Result<Vec<crate::core::rhi::RhiExternalHandle>> {
    use crate::core::rhi::RhiExternalHandle;

    match handle_type {
        SURFACE_HANDLE_TYPE_DMA_BUF => Ok(received_fds
            .iter()
            .zip(plane_sizes.iter())
            .map(|(fd, size)| RhiExternalHandle::DmaBuf {
                fd: *fd,
                size: *size as usize,
            })
            .collect()),
        SURFACE_HANDLE_TYPE_OPAQUE_FD => Ok(received_fds
            .iter()
            .zip(plane_sizes.iter())
            .map(|(fd, size)| RhiExternalHandle::OpaqueFd {
                fd: *fd,
                size: *size as usize,
            })
            .collect()),
        unknown => Err(Error::Configuration(format!(
            "check_out: surface registered with unknown handle type {unknown:?}; the wire \
             carries {SURFACE_HANDLE_TYPE_DMA_BUF:?} or {SURFACE_HANDLE_TYPE_OPAQUE_FD:?}"
        ))),
    }
}

#[cfg(target_os = "macos")]
use crate::apple::xpc_ffi::{
    _NSConcreteMallocBlock, BLOCK_FLAGS_NEEDS_FREE, Block, BlockDescriptor, xpc_connection_cancel,
    xpc_connection_create_mach_service, xpc_connection_resume, xpc_connection_send_message,
    xpc_connection_send_message_with_reply_sync, xpc_connection_set_event_handler,
    xpc_connection_t, xpc_dictionary_copy_mach_send, xpc_dictionary_create,
    xpc_dictionary_get_string, xpc_dictionary_set_mach_send, xpc_dictionary_set_string,
    xpc_error_connection_interrupted, xpc_error_connection_invalid, xpc_is_error, xpc_object_t,
    xpc_release,
};

/// Surface metadata stored alongside the cached pixel buffer.
#[derive(Debug, Clone)]
pub struct CachedSurface {
    /// The resolved pixel buffer.
    pub pixel_buffer: PixelBuffer,
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

    fn insert(&mut self, surface_id: String, pixel_buffer: PixelBuffer) {
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

    fn remove(&mut self, surface_id: &str) {
        self.surfaces.remove(surface_id);
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
/// Rich data backing a [`SurfaceStore`], reached through the store's
/// opaque handle.
///
/// All cross-platform and Linux-specific surface-share IPC methods
/// (`connect`, `check_in`, `check_out`, `register_texture`, etc.)
/// live on this type; the `SurfaceStore` handle forwards each to it.
pub(crate) struct SurfaceStoreInner {
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

impl SurfaceStoreInner {
    /// Create a new surface store (not yet connected). Returns an
    /// `Arc<SurfaceStoreInner>` so the engine can store it directly
    /// and hand [`SurfaceStore`] handles to consumers on demand.
    pub fn new(service_name: String, runtime_id: String) -> Arc<Self> {
        Arc::new(SurfaceStoreInner {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            connection: Mutex::new(None),
            cache: Mutex::new(SurfaceCache::new()),
            checked_in: Mutex::new(CheckedInSurfaces::new()),
            service_name,
            runtime_id,
        })
    }

    /// Connect to the macOS XPC surface-share service.
    ///
    /// This should be called during runtime.start().
    #[cfg(target_os = "macos")]
    pub fn connect(&self) -> Result<()> {
        let service_name = CString::new(self.service_name.as_str())
            .map_err(|e| Error::Configuration(format!("Invalid XPC service name: {}", e)))?;

        let connection = unsafe {
            xpc_connection_create_mach_service(
                service_name.as_ptr(),
                std::ptr::null_mut(), // default queue
                0,                    // no special flags
            )
        };

        if connection.is_null() {
            return Err(Error::Configuration(format!(
                "Failed to create XPC connection to '{}'",
                self.service_name
            )));
        }

        // Set up a minimal event handler (required before resume)
        // We use synchronous send/reply, so the handler just logs connection errors
        unsafe {
            let handler = create_xpc_event_handler();
            xpc_connection_set_event_handler(connection, handler);
            xpc_connection_resume(connection);
        }

        *self.connection.lock() = Some(connection);

        tracing::info!(
            "SurfaceStore: Connected to XPC service '{}'",
            self.service_name
        );

        Ok(())
    }

    /// Disconnect from the surface-share service and release all surfaces.
    ///
    /// This should be called during runtime.stop().
    #[cfg(target_os = "macos")]
    pub fn disconnect(&self) -> Result<()> {
        // Release all checked-in surfaces from the surface-share service
        let surface_ids = self.checked_in.lock().surface_ids();
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
        self.cache.lock().clear();
        self.checked_in.lock().clear();

        // Cancel the XPC connection
        if let Some(connection) = self.connection.lock().take() {
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
    pub fn check_in(&self, pixel_buffer: &PixelBuffer) -> Result<String> {
        use crate::apple::corevideo_ffi::{IOSurfaceGetID, mach_port_deallocate, mach_task_self};

        // Get the IOSurface ID for deduplication
        let pixel_buffer_ref = pixel_buffer.buffer_ref();
        let iosurface = pixel_buffer_ref.iosurface_ref().ok_or_else(|| {
            Error::Configuration("Pixel buffer is not backed by an IOSurface".into())
        })?;
        let iosurface_id = unsafe { IOSurfaceGetID(iosurface) };

        // Check if already checked in
        {
            let checked_in = self.checked_in.lock();
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
        self.cache
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
    pub fn check_out(&self, surface_id: &str) -> Result<PixelBuffer> {
        // Check cache first
        {
            let mut cache = self.cache.lock();
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
            PixelBufferRef, PixelFormat, RhiExternalHandle, RhiPixelBufferImport,
        };

        let handle = RhiExternalHandle::IOSurfaceMachPort { port: mach_port };
        // Width/height/format are extracted from the IOSurface itself after import
        // We pass dummy values as the import will query the actual values from the IOSurface
        let pixel_buffer_ref =
            PixelBufferRef::from_external_handle(handle, 0, 0, PixelFormat::default())?;
        let pixel_buffer = PixelBuffer::new(pixel_buffer_ref);

        // Cache for future use
        self.cache
            .lock()
            .insert(surface_id.to_string(), pixel_buffer.clone());

        Ok(pixel_buffer)
    }

    /// Send check-in request to surface-share service via XPC.
    #[cfg(target_os = "macos")]
    fn check_in_to_surface_share(&self, mach_port: u32) -> Result<String> {
        let connection = self.connection.lock();
        let connection = connection.as_ref().ok_or_else(|| {
            Error::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        // Create request dictionary
        let request = unsafe { xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0) };
        if request.is_null() {
            return Err(Error::Configuration(
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
        let runtime_id_value = CString::new(self.runtime_id.as_str()).unwrap();
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
            return Err(Error::Configuration(
                "XPC check_in: null reply from the surface-share service".into(),
            ));
        }

        // Check for error
        if xpc_is_error(reply) {
            unsafe {
                xpc_release(reply);
            }
            return Err(Error::Configuration(
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
            return Err(Error::Configuration(
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
        let connection = self.connection.lock();
        let connection = connection.as_ref().ok_or_else(|| {
            Error::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        // Create request dictionary
        let request = unsafe { xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0) };
        if request.is_null() {
            return Err(Error::Configuration(
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
            return Err(Error::Configuration(
                "XPC check_out: null reply from the surface-share service".into(),
            ));
        }

        // Check for error
        if xpc_is_error(reply) {
            unsafe {
                xpc_release(reply);
            }
            return Err(Error::Configuration(format!(
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
            return Err(Error::Configuration(format!(
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
    pub fn register_buffer(&self, pool_id: &str, pixel_buffer: &PixelBuffer) -> Result<()> {
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
        let connection = self.connection.lock();
        let connection = connection.as_ref().ok_or_else(|| {
            Error::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        // Create request dictionary
        let request = unsafe { xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0) };
        if request.is_null() {
            return Err(Error::Configuration(
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
        let runtime_id_value = CString::new(self.runtime_id.as_str()).unwrap();
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
            return Err(Error::Configuration(
                "XPC register: null reply from the surface-share service".into(),
            ));
        }

        // Check for error
        if xpc_is_error(reply) {
            unsafe {
                xpc_release(reply);
            }
            return Err(Error::Configuration(
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
            return Err(Error::Configuration(format!("XPC register: {}", error_msg)));
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
    pub fn lookup_buffer(&self, pool_id: &str) -> Result<PixelBuffer> {
        let mach_port = self.lookup_from_surface_share(pool_id)?;

        // Import the pixel buffer from mach port
        use crate::core::rhi::{
            PixelBufferRef, PixelFormat, RhiExternalHandle, RhiPixelBufferImport,
        };

        let handle = RhiExternalHandle::IOSurfaceMachPort { port: mach_port };
        let pixel_buffer_ref =
            PixelBufferRef::from_external_handle(handle, 0, 0, PixelFormat::default())?;
        Ok(PixelBuffer::new(pixel_buffer_ref))
    }

    /// Send lookup request to surface-share service via XPC (new protocol).
    #[cfg(target_os = "macos")]
    fn lookup_from_surface_share(&self, pool_id: &str) -> Result<u32> {
        let connection = self.connection.lock();
        let connection = connection.as_ref().ok_or_else(|| {
            Error::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        // Create request dictionary
        let request = unsafe { xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0) };
        if request.is_null() {
            return Err(Error::Configuration(
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
            return Err(Error::Configuration(
                "XPC lookup: null reply from the surface-share service".into(),
            ));
        }

        // Check for error
        if xpc_is_error(reply) {
            unsafe {
                xpc_release(reply);
            }
            return Err(Error::Configuration(format!(
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
            return Err(Error::Configuration(format!("XPC lookup: {}", error_msg)));
        }

        // Extract mach_port from reply
        let port_key = CString::new("mach_port").unwrap();
        let mach_port = unsafe { xpc_dictionary_copy_mach_send(reply, port_key.as_ptr()) };

        unsafe {
            xpc_release(reply);
        }

        if mach_port == 0 {
            return Err(Error::Configuration(format!(
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
        // Evict the local cache's strong reference first: `check_in` parks a
        // clone there, and a pixel-buffer pool frees a slot only once the
        // buffer's strong count returns to 1 — without this eviction a
        // released surface pins its pool slot for the store's lifetime.
        self.cache.lock().remove(surface_id);
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
            Err(Error::NotSupported(
                "SurfaceStore::release is only supported on macOS and Linux".into(),
            ))
        }
    }

    /// Send release request to surface-share service via XPC.
    #[cfg(target_os = "macos")]
    fn release_from_surface_share(&self, surface_id: &str) -> Result<()> {
        let connection = self.connection.lock();
        let connection = connection.as_ref().ok_or_else(|| {
            Error::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        // Create request dictionary
        let request = unsafe { xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0) };
        if request.is_null() {
            return Err(Error::Configuration(
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
        let runtime_id_value = CString::new(self.runtime_id.as_str()).unwrap();
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
        let stream = std::os::unix::net::UnixStream::connect(&self.service_name).map_err(|e| {
            Error::Configuration(format!(
                "Failed to connect to surface-share socket '{}': {}",
                self.service_name, e
            ))
        })?;

        *self.connection.lock() = Some(stream);

        tracing::info!(
            "SurfaceStore: Connected to surface-share service socket '{}'",
            self.service_name
        );

        Ok(())
    }

    /// Disconnect from the surface-share service and release all surfaces.
    #[cfg(target_os = "linux")]
    pub fn disconnect(&self) -> Result<()> {
        // Release all checked-in surfaces
        let surface_ids = self.checked_in.lock().surface_ids();
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
        self.cache.lock().clear();
        self.checked_in.lock().clear();

        // Drop the connection
        self.connection.lock().take();

        tracing::info!("SurfaceStore: Disconnected from surface-share socket");
        Ok(())
    }

    /// Check in a pixel buffer via Unix socket, returning a surface ID.
    #[cfg(target_os = "linux")]
    pub fn check_in(&self, pixel_buffer: &PixelBuffer) -> Result<String> {
        let exported_planes = exported_plane_wire_handles(pixel_buffer)?;
        let plane_fds = exported_planes.plane_fds;

        let request = serde_json::json!({
            "op": "check_in",
            "runtime_id": self.runtime_id,
            "width": pixel_buffer.width,
            "height": pixel_buffer.height,
            "format": pixel_buffer.format().wire_name(),
            "handle_type": exported_planes.handle_type,
            "plane_sizes": exported_planes.plane_sizes,
            "plane_offsets": exported_planes.plane_offsets,
        });

        let connection = self.connection.lock();
        let stream = connection.as_ref().ok_or_else(|| {
            Error::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        let send_result =
            streamlib_surface_client::send_request_with_fds(stream, &request, &plane_fds, 0);

        // Close the exported fds (surface-share service has its own dups) regardless of
        // the request outcome — the peer owns its kernel-delivered fds and
        // we never keep ours.
        for fd in &plane_fds {
            unsafe { libc::close(*fd) };
        }

        let (response, response_fds) = send_result
            .map_err(|e| Error::Configuration(format!("Unix socket check_in failed: {}", e)))?;
        // check_in never returns fds; close any the surface-share service may have attached
        // defensively so a future protocol drift doesn't leak them.
        for fd in &response_fds {
            unsafe { libc::close(*fd) };
        }

        let surface_id = response
            .get("surface_id")
            .and_then(|v: &serde_json::Value| v.as_str())
            .ok_or_else(|| Error::Configuration("check_in: missing surface_id in response".into()))?
            .to_string();

        self.cache
            .lock()
            .insert(surface_id.clone(), pixel_buffer.clone());

        tracing::debug!("SurfaceStore: Checked in as '{}'", surface_id);

        Ok(surface_id)
    }

    /// Check out a surface by ID via Unix socket.
    #[cfg(target_os = "linux")]
    pub fn check_out(&self, surface_id: &str) -> Result<PixelBuffer> {
        // Check cache first
        {
            let mut cache = self.cache.lock();
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

        let connection = self.connection.lock();
        let stream = connection.as_ref().ok_or_else(|| {
            Error::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        let (response, received_fds) = streamlib_surface_client::send_request_with_fds(
            stream,
            &request,
            &[],
            streamlib_surface_client::MAX_DMA_BUF_PLANES,
        )
        .map_err(|e| Error::Configuration(format!("Unix socket check_out failed: {}", e)))?;

        if let Some(error) = response
            .get("error")
            .and_then(|v: &serde_json::Value| v.as_str())
        {
            for fd in &received_fds {
                unsafe { libc::close(*fd) };
            }
            return Err(Error::Configuration(format!("check_out: {}", error)));
        }

        if received_fds.is_empty() {
            return Err(Error::Configuration(
                "check_out: no DMA-BUF fd in response".into(),
            ));
        }

        // Import every plane under the flavour the service says it registered.
        // The two flavours are different Vulkan external-handle types, so
        // importing an OPAQUE_FD fd as DMA-BUF is a driver-level wrong-import
        // rather than a clean failure.
        use crate::core::rhi::{PixelFormat, RhiPixelBufferImport};

        let plane_sizes: Vec<u64> = response
            .get("plane_sizes")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_u64()).collect())
            .filter(|v: &Vec<u64>| v.len() == received_fds.len())
            .unwrap_or_else(|| vec![0u64; received_fds.len()]);

        let handles = match external_plane_handles_for_flavour(
            response
                .get("handle_type")
                .and_then(|v| v.as_str())
                .unwrap_or(SURFACE_HANDLE_TYPE_DMA_BUF),
            &received_fds,
            &plane_sizes,
        ) {
            Ok(handles) => handles,
            Err(unknown_flavour) => {
                // SAFETY: fds the kernel delivered to this process; the
                // refused import adopted none of them, and nothing else
                // closes them.
                for fd in &received_fds {
                    unsafe { libc::close(*fd) };
                }
                return Err(unknown_flavour);
            }
        };
        let pixel_buffer =
            PixelBuffer::from_external_plane_handles(&handles, 0, 0, PixelFormat::default())?;

        // Cache for future use
        self.cache
            .lock()
            .insert(surface_id.to_string(), pixel_buffer.clone());

        Ok(pixel_buffer)
    }

    /// Send one `register` request over the surface-share socket, passing
    /// `fds` with SCM_RIGHTS under `operation`'s name.
    ///
    /// Takes the fds by value: the service dup'd what it needs during the
    /// send, and `OwnedFd` is what closes this side's copies once — a
    /// registration path that leaks one leaks it per registered surface.
    #[cfg(target_os = "linux")]
    fn send_surface_share_registration(
        &self,
        operation: &str,
        request: &serde_json::Value,
        fds: Vec<std::os::fd::OwnedFd>,
    ) -> Result<()> {
        use std::os::fd::AsRawFd as _;

        let connection = self.connection.lock();
        let stream = connection.as_ref().ok_or_else(|| {
            Error::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        let raw_fds: Vec<std::os::unix::io::RawFd> =
            fds.iter().map(std::os::fd::OwnedFd::as_raw_fd).collect();
        let send_result =
            streamlib_surface_client::send_request_with_fds(stream, request, &raw_fds, 0);
        drop(fds);
        let (response, response_fds) = send_result.map_err(|failure| {
            Error::Configuration(format!("Unix socket {operation} failed: {failure}"))
        })?;
        for fd in &response_fds {
            unsafe { libc::close(*fd) };
        }

        if let Some(error) = response
            .get("error")
            .and_then(|value: &serde_json::Value| value.as_str())
        {
            return Err(Error::Configuration(format!("{operation}: {error}")));
        }
        // A refusal the service reports without an `error` string — a
        // duplicate surface id is the one that matters, because reading it
        // as success would leave every consumer checking out the previous
        // allocation while this one is never refilled.
        if response.get("success").and_then(serde_json::Value::as_bool) == Some(false) {
            return Err(Error::Configuration(format!(
                "{operation}: the surface-share service refused the registration without \
                 naming a reason; the id is most likely already registered"
            )));
        }
        Ok(())
    }

    /// Register a buffer with the surface-share service via Unix socket.
    ///
    /// The plane sizes travel with the registration because a checkout's
    /// importer derives the row pitch from them; a registration without
    /// sizes checks out as an unusable zero-byte plane.
    #[cfg(target_os = "linux")]
    pub fn register_buffer(&self, pool_id: &str, pixel_buffer: &PixelBuffer) -> Result<()> {
        let exported_planes = exported_plane_wire_handles(pixel_buffer)?;

        let request = serde_json::json!({
            "op": "register",
            "surface_id": pool_id,
            "runtime_id": self.runtime_id,
            "width": pixel_buffer.width,
            "height": pixel_buffer.height,
            "format": pixel_buffer.format().wire_name(),
            "resource_type": "pixel_buffer",
            "handle_type": exported_planes.handle_type,
            "plane_sizes": exported_planes.plane_sizes,
            "plane_offsets": exported_planes.plane_offsets,
        });

        self.send_surface_share_registration(
            "register",
            &request,
            // SAFETY: freshly-exported plane fds this process owns.
            exported_planes
                .plane_fds
                .into_iter()
                .map(|plane_fd| unsafe { std::os::fd::OwnedFd::from_raw_fd(plane_fd) })
                .collect(),
        )?;
        tracing::debug!("SurfaceStore: Registered buffer '{}'", pool_id);
        Ok(())
    }

    /// Register a device-export staging buffer and the timeline its
    /// refills signal, so a helper process can check the pair out and
    /// import the staging into an external device API.
    ///
    /// The staging is one flat OPAQUE_FD allocation, never a pixel
    /// buffer: pool allocations are DMA-BUF-flavoured, external device
    /// APIs import OPAQUE_FD, and on NVIDIA one allocation cannot export
    /// both. That is why this cannot go through
    /// [`Self::register_pixel_buffer_with_timeline`], which takes a
    /// [`PixelBuffer`] and its plane handles.
    ///
    /// The refill timeline travels in the `produce_done` slot, and
    /// `consume_done` stays empty. That is the shape, not a compromise:
    /// the host produces the staging's contents and the consumer waits
    /// before reading, which is exactly the `produce_done` edge — and
    /// there is no consumer-side drain for the host to wait on, because
    /// each refill overwrites the staging wholesale.
    #[cfg(target_os = "linux")]
    pub fn register_device_export_staging(
        &self,
        surface_id: &str,
        staging_buffer: &crate::vulkan::rhi::HostVulkanBuffer,
        staging_byte_size: u64,
        width: u32,
        height: u32,
        format: PixelFormat,
        refill_done: &crate::vulkan::rhi::HostVulkanTimelineSemaphore,
    ) -> Result<()> {
        // SAFETY: each export mints a fresh fd this process owns and has
        // handed to no one; adopting it here is what closes it exactly once
        // — including on the early return the timeline export can take.
        let staging_fd =
            unsafe { std::os::fd::OwnedFd::from_raw_fd(staging_buffer.export_opaque_fd_memory()?) };
        let refill_done_fd = unsafe {
            std::os::fd::OwnedFd::from_raw_fd(refill_done.export_opaque_fd().map_err(
                |export_failure| {
                    Error::Configuration(format!(
                        "register_device_export_staging: failed to export the refill timeline: \
                         {export_failure}"
                    ))
                },
            )?)
        };

        let request = serde_json::json!({
            "op": "register",
            "surface_id": surface_id,
            "runtime_id": self.runtime_id,
            "width": width,
            "height": height,
            "format": format.wire_name(),
            "resource_type": "pixel_buffer",
            "handle_type": SURFACE_HANDLE_TYPE_OPAQUE_FD,
            "plane_sizes": [staging_byte_size],
            "plane_offsets": [0],
            "has_produce_done_fd": true,
            "has_consume_done_fd": false,
        });

        self.send_surface_share_registration(
            "register_device_export_staging",
            &request,
            vec![staging_fd, refill_done_fd],
        )?;
        tracing::debug!(
            "SurfaceStore: Registered device-export staging '{}' ({} bytes)",
            surface_id,
            staging_byte_size,
        );
        Ok(())
    }

    /// Register a texture with the surface-share service via Unix socket.
    ///
    /// `timeline` — when `Some`, the host's exportable timeline semaphore is
    /// exported as an OPAQUE_FD and shipped alongside the DMA-BUF FD. The
    /// surface-share service stores the FD; subprocess Vulkan adapters
    /// `check_out` it via [`streamlib_adapter_vulkan::VulkanSurfaceAdapter`]
    /// and import it through `HostVulkanTimelineSemaphore::from_imported_opaque_fd`,
    /// reusing the host adapter's timeline-wait + signal path (#531). `None`
    /// for adapters that don't need explicit Vulkan sync (OpenGL — its
    /// `glFinish` + DMA-BUF kernel-fence semantics carry visibility).
    ///
    /// Dispatches internally on the texture's underlying memory flavor:
    ///
    /// - **DMA-BUF** (the default; tiled `VkImage` with a DRM format
    ///   modifier OR linear `VkBuffer`-backed surface) — exports a
    ///   DMA-BUF FD and publishes with `handle_type: "dma_buf"`.
    /// - **OPAQUE_FD** (`HostVulkanTexture::new_opaque_fd_export` —
    ///   DEVICE_LOCAL `VkImage`, `VK_IMAGE_TILING_OPTIMAL`, no DRM
    ///   modifier, format restricted to `Rgba8Unorm` / `Rgba16Float` /
    ///   `Rgba32Float`) — exports the OPAQUE_FD memory handle and
    ///   publishes with `handle_type: "opaque_fd"` plus the
    ///   `vk_image_*` round-trip fields (#806) the consumer needs to
    ///   rebuild a byte-for-byte matching `VkImageCreateInfo` for the
    ///   CUDA `cudaExternalMemoryGetMappedMipmappedArray` import path.
    #[cfg(target_os = "linux")]
    pub fn register_texture(
        &self,
        surface_id: &str,
        texture: &crate::core::rhi::Texture,
        produce_done: Option<&crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
        consume_done: Option<&crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
        current_image_layout: streamlib_consumer_rhi::VulkanLayout,
    ) -> Result<()> {
        let is_opaque_fd = texture.vulkan_inner().is_opaque_fd_export();

        // Export the memory FD per the texture's underlying memory
        // flavor. OPAQUE_FD textures have no DMA-BUF export path on
        // NVIDIA (and the call would fail at the driver); DMA-BUF
        // textures have no OPAQUE_FD export path with VMA's
        // per-pool memory configuration.
        let fd = if is_opaque_fd {
            texture.vulkan_inner().export_opaque_fd_memory()?
        } else {
            texture.vulkan_inner().export_dma_buf_fd()?
        };

        // Optionally export the producer-side `produce_done` and
        // consumer-side `consume_done` timeline-semaphores as
        // OPAQUE_FDs (single-writer-per-edge model — see
        // `docs/architecture/adapter-timeline-single-writer.md`). The
        // host retains ownership of both semaphore objects; the FDs are
        // duplicated by the kernel during SCM_RIGHTS and we close our
        // copies after send.
        let produce_done_fd: Option<std::os::unix::io::RawFd> = match produce_done {
            Some(t) => match t.export_opaque_fd() {
                Ok(f) => Some(f),
                Err(e) => {
                    unsafe { libc::close(fd) };
                    return Err(Error::Configuration(format!(
                        "register_texture: failed to export produce_done opaque fd: {}",
                        e
                    )));
                }
            },
            None => None,
        };
        let consume_done_fd: Option<std::os::unix::io::RawFd> = match consume_done {
            Some(t) => match t.export_opaque_fd() {
                Ok(f) => Some(f),
                Err(e) => {
                    unsafe { libc::close(fd) };
                    if let Some(p) = produce_done_fd {
                        unsafe { libc::close(p) };
                    }
                    return Err(Error::Configuration(format!(
                        "register_texture: failed to export consume_done opaque fd: {}",
                        e
                    )));
                }
            },
            None => None,
        };

        // Per-flavor wire fields. The DMA-BUF path carries the DRM
        // modifier + per-plane layout the EGL / Vulkan import paths
        // need. The OPAQUE_FD path carries the `vk_image_*` round-trip
        // shape `cudaExternalMemoryGetMappedMipmappedArray` requires —
        // matches the fixed shape `HostVulkanTexture::new_opaque_fd_export`
        // hardcodes (2D, mipLevels=1, arrayLayers=1, samples=1,
        // tiling=OPTIMAL, usage=TRANSFER_SRC|TRANSFER_DST|SAMPLED|STORAGE).
        let request = if is_opaque_fd {
            let allocation_size = texture.vulkan_inner().vma_allocation_size() as u64;
            const VK_IMAGE_TYPE_2D: i32 = 1;
            const VK_IMAGE_TILING_OPTIMAL: i32 = 0;
            const VK_SAMPLE_COUNT_1: i32 = 1;
            const VK_IMAGE_USAGE_TRANSFER_SRC_BIT: u32 = 0x0000_0001;
            const VK_IMAGE_USAGE_TRANSFER_DST_BIT: u32 = 0x0000_0002;
            const VK_IMAGE_USAGE_SAMPLED_BIT: u32 = 0x0000_0004;
            const VK_IMAGE_USAGE_STORAGE_BIT: u32 = 0x0000_0008;
            let vk_image_usage = VK_IMAGE_USAGE_TRANSFER_SRC_BIT
                | VK_IMAGE_USAGE_TRANSFER_DST_BIT
                | VK_IMAGE_USAGE_SAMPLED_BIT
                | VK_IMAGE_USAGE_STORAGE_BIT;

            serde_json::json!({
                "op": "register",
                "surface_id": surface_id,
                "runtime_id": self.runtime_id,
                "width": texture.width(),
                "height": texture.height(),
                "format": format!("{:?}", texture.format()),
                "resource_type": "texture",
                "handle_type": "opaque_fd",
                "plane_sizes": [allocation_size],
                "plane_offsets": [0u64],
                "plane_strides": [0u64],
                "drm_format_modifier": 0u64,
                "has_produce_done_fd": produce_done_fd.is_some(),
                "has_consume_done_fd": consume_done_fd.is_some(),
                "current_image_layout": current_image_layout.as_vk().as_raw(),
                "vk_image_type": VK_IMAGE_TYPE_2D,
                "vk_image_mip_levels": 1u32,
                "vk_image_array_layers": 1u32,
                "vk_image_samples": VK_SAMPLE_COUNT_1,
                "vk_image_tiling": VK_IMAGE_TILING_OPTIMAL,
                "vk_image_usage": vk_image_usage,
                "vk_image_allocation_size": allocation_size,
            })
        } else {
            // Carry the DRM format modifier and per-plane row pitch so
            // the consumer-side EGL or Vulkan import can pass them via
            // EGL_DMA_BUF_PLANE0_MODIFIER_LO/HI_EXT and
            // EGL_DMA_BUF_PLANE{N}_PITCH_EXT (or
            // VkImageDrmFormatModifierExplicitCreateInfoEXT). Zero
            // modifier means LINEAR / not applicable; render-target
            // consumers must refuse such surfaces because LINEAR
            // DMA-BUFs are sampler-only on NVIDIA (see
            // docs/learnings/nvidia-egl-dmabuf-render-target.md).
            let drm_format_modifier = texture.vulkan_inner().chosen_drm_format_modifier();
            let plane_layout = texture
                .vulkan_inner()
                .dma_buf_plane_layout()
                .unwrap_or_else(|_| vec![(0, 0)]);
            let plane_offsets: Vec<u64> = plane_layout.iter().map(|(o, _)| *o).collect();
            let plane_strides: Vec<u64> = plane_layout.iter().map(|(_, s)| *s).collect();

            serde_json::json!({
                "op": "register",
                "surface_id": surface_id,
                "runtime_id": self.runtime_id,
                "width": texture.width(),
                "height": texture.height(),
                "format": format!("{:?}", texture.format()),
                "resource_type": "texture",
                "plane_offsets": plane_offsets,
                "plane_strides": plane_strides,
                "drm_format_modifier": drm_format_modifier,
                "has_produce_done_fd": produce_done_fd.is_some(),
                "has_consume_done_fd": consume_done_fd.is_some(),
                // The producer's declared `VkImageLayout`: the
                // layout the texture lives in immediately after
                // registration, fed to host consumers as the source
                // layout of their first QFOT acquire barrier. Encoded
                // as i32 per the Vulkan spec.
                "current_image_layout": current_image_layout.as_vk().as_raw(),
            })
        };

        let mut fds: Vec<std::os::unix::io::RawFd> = vec![fd];
        if let Some(s) = produce_done_fd {
            fds.push(s);
        }
        if let Some(s) = consume_done_fd {
            fds.push(s);
        }
        self.send_surface_share_registration(
            "register_texture",
            &request,
            adopt_exported_fds(fds),
        )?;

        tracing::debug!(
            "SurfaceStore: Registered texture '{}' (produce_done={}, consume_done={})",
            surface_id,
            produce_done.is_some(),
            consume_done.is_some(),
        );
        Ok(())
    }

    /// Register a host-allocated multi-plane pixel buffer with the
    /// surface-share service under an explicit `surface_id`, optionally
    /// shipping the host's exportable timeline semaphore alongside as an
    /// OPAQUE_FD.
    ///
    /// Distinct from [`Self::register_buffer`] (single-plane, no
    /// timeline) and [`Self::register_texture`] (image, with optional
    /// timeline). This is the cpu-readback adapter's registration path:
    /// the host pre-allocates one HOST_VISIBLE / HOST_COHERENT linear
    /// staging `VkBuffer` per plane and an exportable timeline; the
    /// subprocess `check_out`s the bundle once at registration time and
    /// imports each plane via [`streamlib_consumer_rhi::ConsumerVulkanBuffer::from_dma_buf_fds`]
    /// + the timeline via [`streamlib_consumer_rhi::ConsumerVulkanTimelineSemaphore::from_imported_opaque_fd`].
    ///
    /// Per-acquire IPC after registration is a thin trigger that
    /// signals a new timeline value on the same shared timeline; no
    /// further FD passing is needed.
    #[cfg(target_os = "linux")]
    pub fn register_pixel_buffer_with_timeline(
        &self,
        surface_id: &str,
        pixel_buffer: &PixelBuffer,
        produce_done: Option<&crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
        consume_done: Option<&crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
    ) -> Result<()> {
        let exported_planes = exported_plane_wire_handles(pixel_buffer)?;
        let plane_fds = exported_planes.plane_fds;

        // Export `produce_done` + `consume_done` as OPAQUE_FDs (the
        // single-writer-per-edge pair documented in
        // `docs/architecture/adapter-timeline-single-writer.md`). The
        // surface-share daemon's wire format peels both trailing FDs
        // in the published order.
        let produce_done_fd: Option<std::os::unix::io::RawFd> = match produce_done {
            Some(t) => match t.export_opaque_fd() {
                Ok(f) => Some(f),
                Err(e) => {
                    for fd in &plane_fds {
                        unsafe { libc::close(*fd) };
                    }
                    return Err(Error::Configuration(format!(
                        "register_pixel_buffer_with_timeline: failed to export produce_done opaque fd: {}",
                        e
                    )));
                }
            },
            None => None,
        };
        let consume_done_fd: Option<std::os::unix::io::RawFd> = match consume_done {
            Some(t) => match t.export_opaque_fd() {
                Ok(f) => Some(f),
                Err(e) => {
                    for fd in &plane_fds {
                        unsafe { libc::close(*fd) };
                    }
                    if let Some(p) = produce_done_fd {
                        unsafe { libc::close(p) };
                    }
                    return Err(Error::Configuration(format!(
                        "register_pixel_buffer_with_timeline: failed to export consume_done opaque fd: {}",
                        e
                    )));
                }
            },
            None => None,
        };

        let request = serde_json::json!({
            "op": "register",
            "surface_id": surface_id,
            "runtime_id": self.runtime_id,
            "width": pixel_buffer.width,
            "height": pixel_buffer.height,
            "format": pixel_buffer.format().wire_name(),
            "resource_type": "pixel_buffer",
            "handle_type": exported_planes.handle_type,
            "plane_sizes": exported_planes.plane_sizes,
            "plane_offsets": exported_planes.plane_offsets,
            "has_produce_done_fd": produce_done_fd.is_some(),
            "has_consume_done_fd": consume_done_fd.is_some(),
        });

        let mut fds: Vec<std::os::unix::io::RawFd> = plane_fds.clone();
        if let Some(s) = produce_done_fd {
            fds.push(s);
        }
        if let Some(s) = consume_done_fd {
            fds.push(s);
        }
        self.send_surface_share_registration(
            "register_pixel_buffer_with_timeline",
            &request,
            adopt_exported_fds(fds),
        )?;

        tracing::debug!(
            "SurfaceStore: Registered pixel buffer '{}' ({} plane(s), produce_done={}, consume_done={})",
            surface_id,
            exported_planes.plane_sizes.len(),
            produce_done.is_some(),
            consume_done.is_some(),
        );
        Ok(())
    }

    /// Lookup a buffer from the surface-share service via Unix socket.
    ///
    /// Checks the host-local cache first so producers that `check_in`'d the
    /// buffer in the same process (e.g. the escalate-on-behalf flow) skip the
    /// per-frame unix-socket round-trip and DMA-BUF re-import.
    #[cfg(target_os = "linux")]
    pub fn lookup_buffer(&self, pool_id: &str) -> Result<PixelBuffer> {
        {
            let cache = self.cache.lock();
            if let Some(cached) = cache.surfaces.get(pool_id) {
                return Ok(cached.pixel_buffer.clone());
            }
        }

        let request = serde_json::json!({
            "op": "lookup",
            "surface_id": pool_id,
        });

        let connection = self.connection.lock();
        let stream = connection.as_ref().ok_or_else(|| {
            Error::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        let (response, received_fds) = streamlib_surface_client::send_request_with_fds(
            stream,
            &request,
            &[],
            streamlib_surface_client::MAX_DMA_BUF_PLANES,
        )
        .map_err(|e| Error::Configuration(format!("Unix socket lookup failed: {}", e)))?;

        if let Some(error) = response
            .get("error")
            .and_then(|v: &serde_json::Value| v.as_str())
        {
            for fd in &received_fds {
                unsafe { libc::close(*fd) };
            }
            return Err(Error::Configuration(format!("lookup: {}", error)));
        }

        if received_fds.is_empty() {
            return Err(Error::Configuration(
                "lookup: no memory fd in response".into(),
            ));
        }

        // Dispatch on the wire-level handle type. OPAQUE_FD lookups can't
        // construct a host-side `PixelBuffer` (that import path is
        // DMA-BUF-only — see `RhiPixelBufferImport::from_external_plane_handles`).
        // Subprocess consumers go through `streamlib-surface-client` directly
        // and import via `streamlib_consumer_rhi::ConsumerVulkanBuffer::from_opaque_fd`.
        let handle_type = response
            .get("handle_type")
            .and_then(|v| v.as_str())
            .unwrap_or("dma_buf");

        if handle_type == "opaque_fd" {
            for fd in &received_fds {
                unsafe { libc::close(*fd) };
            }
            return Err(Error::NotSupported(
                "SurfaceStore::lookup_buffer: surface registered with \
                 handle_type=\"opaque_fd\"; the host-side PixelBuffer \
                 import path is DMA-BUF-only. Subprocess consumers should \
                 use streamlib-surface-client directly + \
                 ConsumerVulkanBuffer::from_opaque_fd."
                    .into(),
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
        PixelBuffer::from_external_plane_handles(&handles, 0, 0, PixelFormat::default())
    }

    /// Publish a producer's post-release `VkImageLayout` for the given
    /// `surface_id`. Issued through the surface-share `update_layout`
    /// op (#633): producers call this immediately after their QFOT
    /// release barrier records, so the next consumer's `lookup_texture`
    /// sees the post-release layout instead of the previous one.
    /// Returns `Ok(())` on success; `Err` on socket failure or wire
    /// rejection (e.g., unknown surface_id).
    #[cfg(target_os = "linux")]
    pub fn update_image_layout(
        &self,
        surface_id: &str,
        layout: streamlib_consumer_rhi::VulkanLayout,
    ) -> Result<()> {
        let request = serde_json::json!({
            "op": "update_layout",
            "surface_id": surface_id,
            "current_image_layout": layout.as_vk().as_raw(),
        });

        let connection = self.connection.lock();
        let stream = connection.as_ref().ok_or_else(|| {
            Error::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        let (response, response_fds) =
            streamlib_surface_client::send_request_with_fds(stream, &request, &[], 0).map_err(
                |e| Error::Configuration(format!("Unix socket update_layout failed: {}", e)),
            )?;
        for f in &response_fds {
            unsafe { libc::close(*f) };
        }

        if let Some(error) = response.get("error").and_then(|v| v.as_str()) {
            return Err(Error::Configuration(format!("update_layout: {}", error)));
        }

        match response.get("success").and_then(|v| v.as_bool()) {
            Some(true) => Ok(()),
            Some(false) => Err(Error::Configuration(format!(
                "update_layout: surface_id '{}' not registered",
                surface_id
            ))),
            None => Err(Error::Configuration(
                "update_layout: malformed response (missing `success`)".into(),
            )),
        }
    }

    /// Lookup a texture from the surface-share service via Unix socket.
    /// Returns the imported [`Texture`] paired with the
    /// producer's last-published `current_image_layout`. Cross-process
    /// consumers feed the layout into the source layout of their first
    /// QFOT acquire barrier (#633).
    #[cfg(target_os = "linux")]
    pub fn lookup_texture(
        &self,
        surface_id: &str,
    ) -> Result<(
        crate::core::rhi::Texture,
        streamlib_consumer_rhi::VulkanLayout,
    )> {
        let request = serde_json::json!({
            "op": "lookup",
            "surface_id": surface_id,
        });

        let connection = self.connection.lock();
        let stream = connection.as_ref().ok_or_else(|| {
            Error::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        let (response, received_fds) = streamlib_surface_client::send_request_with_fds(
            stream,
            &request,
            &[],
            streamlib_surface_client::MAX_DMA_BUF_PLANES,
        )
        .map_err(|e| Error::Configuration(format!("Unix socket lookup_texture failed: {}", e)))?;

        if let Some(error) = response
            .get("error")
            .and_then(|v: &serde_json::Value| v.as_str())
        {
            for fd in &received_fds {
                unsafe { libc::close(*fd) };
            }
            return Err(Error::Configuration(format!("lookup_texture: {}", error)));
        }

        if received_fds.is_empty() {
            return Err(Error::Configuration(
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
                Error::Configuration("lookup_texture: missing width in response".into())
            })? as u32;

        let height = response
            .get("height")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                Error::Configuration("lookup_texture: missing height in response".into())
            })? as u32;

        let format_str = response
            .get("format")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                Error::Configuration("lookup_texture: missing format in response".into())
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
                return Err(Error::Configuration(format!(
                    "lookup_texture: unknown format '{}'",
                    format_str
                )));
            }
        };

        let allocation_size = (width as u64) * (height as u64) * (format.bytes_per_pixel() as u64);

        let vulkan_device = crate::vulkan::rhi::vulkan_buffer::VULKAN_DEVICE_FOR_IMPORT
            .get()
            .ok_or_else(|| {
                Error::NotSupported(
                    "lookup_texture: HostVulkanDevice not initialized for import".into(),
                )
            })?;

        let vulkan_texture = crate::vulkan::rhi::HostVulkanTexture::from_dma_buf_fd(
            vulkan_device,
            dma_buf_fd,
            width,
            height,
            format,
            allocation_size,
        )?;

        // Parse the producer's last-published `VkImageLayout` from the
        // response (#633). Absent or unparseable defaults to UNDEFINED
        // — back-compat for surface-share daemons / clients that haven't
        // been updated yet.
        let current_image_layout = response
            .get("current_image_layout")
            .and_then(|v| v.as_i64())
            .map(|raw| streamlib_consumer_rhi::VulkanLayout(raw as i32))
            .unwrap_or(streamlib_consumer_rhi::VulkanLayout::UNDEFINED);

        Ok((
            crate::core::rhi::Texture::from_vulkan(vulkan_texture),
            current_image_layout,
        ))
    }

    /// Send release request to surface-share service via Unix socket.
    #[cfg(target_os = "linux")]
    fn release_from_surface_share_unix(&self, surface_id: &str) -> Result<()> {
        let request = serde_json::json!({
            "op": "release",
            "surface_id": surface_id,
            "runtime_id": self.runtime_id,
        });

        let connection = self.connection.lock();
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
        Err(Error::NotSupported(
            "SurfaceStore is only supported on macOS and Linux".into(),
        ))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn disconnect(&self) -> Result<()> {
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn check_in(&self, _pixel_buffer: &PixelBuffer) -> Result<String> {
        Err(Error::NotSupported(
            "SurfaceStore is only supported on macOS and Linux".into(),
        ))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn check_out(&self, _surface_id: &str) -> Result<PixelBuffer> {
        Err(Error::NotSupported(
            "SurfaceStore is only supported on macOS and Linux".into(),
        ))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn register_buffer(&self, _pool_id: &str, _pixel_buffer: &PixelBuffer) -> Result<()> {
        Err(Error::NotSupported(
            "SurfaceStore is only supported on macOS and Linux".into(),
        ))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn lookup_buffer(&self, _pool_id: &str) -> Result<PixelBuffer> {
        Err(Error::NotSupported(
            "SurfaceStore is only supported on macOS and Linux".into(),
        ))
    }

    // Non-Linux stubs. `_current_image_layout` and the (timeline,
    // layout) tuple shape mirror the Linux signatures so a future
    // non-Linux caller hits the same API surface; they always error
    // because the surface-share daemon and texture import paths are
    // Linux-only today. Layout is `i32` rather than `VulkanLayout`
    // because `VulkanLayout` is itself Linux-only.
    #[cfg(not(target_os = "linux"))]
    pub fn register_texture(
        &self,
        _surface_id: &str,
        _texture: &crate::core::rhi::Texture,
        _timeline: Option<&()>,
        _current_image_layout: i32,
    ) -> Result<()> {
        Err(Error::NotSupported(
            "Texture registration not supported on this platform".into(),
        ))
    }

    #[cfg(not(target_os = "linux"))]
    pub fn lookup_texture(&self, _surface_id: &str) -> Result<(crate::core::rhi::Texture, i32)> {
        Err(Error::NotSupported(
            "Texture lookup not supported on this platform".into(),
        ))
    }

    #[cfg(not(target_os = "linux"))]
    pub fn update_image_layout(&self, _surface_id: &str, _layout: i32) -> Result<()> {
        Err(Error::NotSupported(
            "update_image_layout not supported on this platform".into(),
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

impl std::fmt::Debug for SurfaceStoreInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SurfaceStoreInner")
            .field("service_name", &self.service_name)
            .field("runtime_id", &self.runtime_id)
            .finish()
    }
}

// =============================================================================
// SurfaceStore
// =============================================================================
//
// Public handle wrapping `Arc<SurfaceStoreInner>`. Every method
// borrows the inner and invokes it directly.

use std::ffi::c_void as ss_c_void;

/// Cross-process surface sharing handle.
///
/// Cheap to clone — increments the strong count on the host's
/// `Arc<SurfaceStoreInner>`. Both XPC (macOS) and Unix socket (Linux)
/// variants are exposed through the same method surface.
pub struct SurfaceStore {
    /// Opaque handle to the host's `Arc<SurfaceStoreInner>`.
    pub(crate) handle: *const ss_c_void,
}

// SAFETY: `handle` points at an `Arc<SurfaceStoreInner>` whose
// interior is Send+Sync (Mutex-protected state, plus String fields).
unsafe impl Send for SurfaceStore {}
unsafe impl Sync for SurfaceStore {}

impl SurfaceStore {
    /// Create a new SurfaceStore handle (not yet connected). The
    /// underlying [`SurfaceStoreInner`] is allocated as an
    /// `Arc<SurfaceStoreInner>` and wrapped behind the opaque handle.
    /// Engine and integration
    /// tests use this; the runtime's `start()` path uses the
    /// `from_arc_into_raw` helper directly so it can share the Arc
    /// with `GpuContext::set_surface_store`.
    pub fn new(service_name: String, runtime_id: String) -> Self {
        Self::from_arc_into_raw(SurfaceStoreInner::new(service_name, runtime_id))
    }

    /// Internal helper: leak an initial Arc strong count via
    /// `Arc::into_raw` and wrap it as the opaque handle.
    pub(crate) fn from_arc_into_raw(arc: Arc<SurfaceStoreInner>) -> Self {
        let handle = Arc::into_raw(arc) as *const ss_c_void;
        Self { handle }
    }

    /// Build a null-handle "None" sentinel for the
    /// `GpuContext::surface_store()` API's `Option<SurfaceStore>`
    /// shape. `SurfaceStore::is_none()` returns `true` for such a
    /// value and `Drop` short-circuits on null handle.
 
    /// Whether this is a null-handle sentinel (the "None" branch of
    /// the `Option<SurfaceStore>` return shape).
    pub(crate) fn is_none(&self) -> bool {
        self.handle.is_null()
    }

    /// Engine-internal borrow of the host-owned `SurfaceStoreInner`.
    pub(crate) fn host_inner(&self) -> &SurfaceStoreInner {
        // SAFETY: `self.handle` is `Arc::into_raw(Arc<SurfaceStoreInner>)`.
        unsafe { &*(self.handle as *const SurfaceStoreInner) }
    }

    /// Connect to the surface-share service (XPC on macOS, Unix
    /// socket on Linux).
    pub fn connect(&self) -> Result<()> {
        if self.is_none() {
            return Err(Error::Configuration(
                "SurfaceStore::connect: null handle".into(),
            ));
        }
        self.host_inner().connect()
    }

    /// Disconnect from the surface-share service.
    pub fn disconnect(&self) -> Result<()> {
        if self.is_none() {
            return Err(Error::Configuration(
                "SurfaceStore::disconnect: null handle".into(),
            ));
        }
        self.host_inner().disconnect()
    }

    /// Check in a pixel buffer for cross-process sharing.
    pub fn check_in(&self, pixel_buffer: &PixelBuffer) -> Result<String> {
        if self.is_none() {
            return Err(Error::Configuration(
                "SurfaceStore::check_in: null handle".into(),
            ));
        }
        self.host_inner().check_in(pixel_buffer)
    }

    /// Check out a surface by its surface_id.
    pub fn check_out(&self, surface_id: &str) -> Result<PixelBuffer> {
        if self.is_none() {
            return Err(Error::Configuration(
                "SurfaceStore::check_out: null handle".into(),
            ));
        }
        self.host_inner().check_out(surface_id)
    }

    /// Register a pre-allocated buffer under the given pool id.
    pub fn register_buffer(&self, pool_id: &str, pixel_buffer: &PixelBuffer) -> Result<()> {
        if self.is_none() {
            return Err(Error::Configuration(
                "SurfaceStore::register_buffer: null handle".into(),
            ));
        }
        self.host_inner().register_buffer(pool_id, pixel_buffer)
    }

    /// Look up a previously-registered buffer by its pool id.
    pub fn lookup_buffer(&self, pool_id: &str) -> Result<PixelBuffer> {
        if self.is_none() {
            return Err(Error::Configuration(
                "SurfaceStore::lookup_buffer: null handle".into(),
            ));
        }
        self.host_inner().lookup_buffer(pool_id)
    }

    /// Release a checked-out surface by its surface_id.
    pub fn release(&self, surface_id: &str) -> Result<()> {
        if self.is_none() {
            return Err(Error::Configuration(
                "SurfaceStore::release: null handle".into(),
            ));
        }
        self.host_inner().release(surface_id)
    }

    /// **Engine-only** — public surface lives on the
    /// [`crate::host_rhi::HostSurfaceStoreExt`] extension trait
    /// (`register_texture`). The parameter type
    /// `Option<&HostVulkanTimelineSemaphore>` is host-internal —
    /// cdylib subprocess customers cannot construct it and so cannot
    /// call this through typed Rust; the engine-only extension
    /// trait makes that constraint explicit at the type-system
    /// layer (mirrors [`crate::host_rhi::HostTextureExt`]
    /// /[`crate::host_rhi::HostPixelBufferRefExt`]).
    #[cfg(target_os = "linux")]
    pub(crate) fn host_register_texture(
        &self,
        surface_id: &str,
        texture: &crate::core::rhi::Texture,
        produce_done: Option<&crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
        consume_done: Option<&crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
        current_image_layout: streamlib_consumer_rhi::VulkanLayout,
    ) -> Result<()> {
        if self.is_none() {
            return Err(Error::Configuration(
                "SurfaceStore::register_texture: null handle".into(),
            ));
        }
        self.host_inner().register_texture(surface_id, texture, produce_done, consume_done, current_image_layout)
    }

    /// **Engine-only** — public surface lives on the
    /// [`crate::host_rhi::HostSurfaceStoreExt`] extension trait
    /// (`register_pixel_buffer_with_timeline`). Same engine-only
    /// rationale as [`Self::host_register_texture`].
    #[cfg(target_os = "linux")]
    pub(crate) fn host_register_pixel_buffer_with_timeline(
        &self,
        surface_id: &str,
        pixel_buffer: &PixelBuffer,
        produce_done: Option<&crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
        consume_done: Option<&crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
    ) -> Result<()> {
        if self.is_none() {
            return Err(Error::Configuration(
                "SurfaceStore::register_pixel_buffer_with_timeline: null handle".into(),
            ));
        }
        self.host_inner().register_pixel_buffer_with_timeline(surface_id, pixel_buffer, produce_done, consume_done)
    }

    /// Look up a registered texture by surface_id (Linux).
    #[cfg(target_os = "linux")]
    pub fn lookup_texture(
        &self,
        surface_id: &str,
    ) -> Result<(
        crate::core::rhi::Texture,
        streamlib_consumer_rhi::VulkanLayout,
    )> {
        if self.is_none() {
            return Err(Error::Configuration(
                "SurfaceStore::lookup_texture: null handle".into(),
            ));
        }
        self.host_inner().lookup_texture(surface_id)
    }

    /// Update the published `VkImageLayout` for a registered texture (Linux).
    #[cfg(target_os = "linux")]
    pub fn update_image_layout(
        &self,
        surface_id: &str,
        layout: streamlib_consumer_rhi::VulkanLayout,
    ) -> Result<()> {
        if self.is_none() {
            return Err(Error::Configuration(
                "SurfaceStore::update_image_layout: null handle".into(),
            ));
        }
        self.host_inner().update_image_layout(surface_id, layout)
    }
}

impl Clone for SurfaceStore {
    fn clone(&self) -> Self {
        if !self.is_none() {
            // SAFETY: `handle` is `Arc::into_raw(Arc<SurfaceStoreInner>)`
            // (see `from_arc_into_raw`); balanced by the Drop impl below.
            unsafe {
                Arc::increment_strong_count(self.handle as *const SurfaceStoreInner);
            }
        }
        Self {
            handle: self.handle,
        }
    }
}

impl Drop for SurfaceStore {
    fn drop(&mut self) {
        if !self.is_none() {
            // SAFETY: matched with `Arc::into_raw` in `from_arc_into_raw`
            // and any `Clone` increment.
            unsafe {
                Arc::decrement_strong_count(self.handle as *const SurfaceStoreInner);
            }
        }
    }
}

impl std::fmt::Debug for SurfaceStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SurfaceStore")
            .field("handle", &self.handle)
            .finish()
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests_ss {
    use super::*;

    #[test]
    fn surface_store_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SurfaceStore>();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod external_handle_flavour_tests {
    use super::*;
    use crate::core::rhi::RhiExternalHandle;

    /// An OPAQUE_FD-registered surface must import as OPAQUE_FD. This path is
    /// reachable the moment a helper process checks out device-export staging,
    /// and the failure it prevents is a wrong-type import inside the driver
    /// rather than an error the caller could see.
    #[test]
    fn an_opaque_fd_surface_imports_as_opaque_fd() {
        let handles =
            external_plane_handles_for_flavour(SURFACE_HANDLE_TYPE_OPAQUE_FD, &[7, 8], &[64, 128])
                .unwrap();
        assert!(matches!(
            handles.as_slice(),
            [
                RhiExternalHandle::OpaqueFd { fd: 7, size: 64 },
                RhiExternalHandle::OpaqueFd { fd: 8, size: 128 },
            ]
        ));
    }

    #[test]
    fn a_dma_buf_surface_imports_as_dma_buf() {
        let handles =
            external_plane_handles_for_flavour(SURFACE_HANDLE_TYPE_DMA_BUF, &[3], &[4096]).unwrap();
        assert!(matches!(
            handles.as_slice(),
            [RhiExternalHandle::DmaBuf { fd: 3, size: 4096 }]
        ));
    }

    /// A surface registered before the flavour was expressible sends no
    /// `handle_type`; the caller substitutes the DMA-BUF default, so every
    /// such surface keeps importing exactly as it did.
    #[test]
    fn the_absent_flavour_default_is_dma_buf() {
        let handles =
            external_plane_handles_for_flavour(SURFACE_HANDLE_TYPE_DMA_BUF, &[3], &[0]).unwrap();
        assert!(matches!(
            handles.as_slice(),
            [RhiExternalHandle::DmaBuf { fd: 3, .. }]
        ));
    }

    #[test]
    fn an_unknown_flavour_is_refused_by_name() {
        let refusal = external_plane_handles_for_flavour("io_surface", &[3], &[0]).unwrap_err();
        assert!(
            refusal.to_string().contains("io_surface"),
            "the refusal must name what it got: {refusal}"
        );
    }
}
