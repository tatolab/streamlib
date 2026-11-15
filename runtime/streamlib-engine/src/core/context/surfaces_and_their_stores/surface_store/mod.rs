// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Surface Store for cross-process GPU surface sharing.
//!
//! Provides check-in/check-out semantics against the per-runtime surface-share
//! service. Surfaces are cached locally after first checkout to minimize
//! round-trips.

use std::sync::Arc;

use parking_lot::Mutex;

#[cfg(target_os = "linux")]
use std::os::fd::OwnedFd;

use crate::core::rhi::PixelBuffer;
#[cfg(target_os = "linux")]
use crate::core::rhi::PixelFormat;
use crate::core::{Error, Result};

use crate::core::context::surfaces_and_their_stores::surface_check_out_lease_registry::SurfaceCheckOutLeaseRegistry;

#[cfg(target_os = "macos")]
mod mach_surface_share_client;
#[cfg(target_os = "linux")]
mod registration_payloads;
mod surface_cache;
#[cfg(target_os = "linux")]
mod unix_socket_surface_share_client;
#[cfg(target_os = "linux")]
mod unix_socket_surface_share_replies;

use surface_cache::SurfaceCache;

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

/// The id a `check_in` answer names for the surface it registered.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn surface_id_of_check_in_answer(answer: &serde_json::Value) -> Result<String> {
    answer
        .get("surface_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| Error::Configuration("check_in: missing surface_id in response".into()))
}

/// Hold a registration answer to what it says: an `error` string is a
/// refusal, and so is `success: false` without one — a duplicate surface id is
/// the one that matters, because reading it as success would leave every
/// consumer checking out the previous allocation while this one is never
/// refilled.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn refusal_of_a_registration_answer(operation: &str, answer: &serde_json::Value) -> Result<()> {
    if let Some(error) = answer.get("error").and_then(serde_json::Value::as_str) {
        return Err(Error::Configuration(format!("{operation}: {error}")));
    }
    if answer.get("success").and_then(serde_json::Value::as_bool) == Some(false) {
        return Err(Error::Configuration(format!(
            "{operation}: the surface-share service refused the registration without \
             naming a reason; the id is most likely already registered"
        )));
    }
    Ok(())
}

/// A looked-up DMA-BUF registration, not imported: its fd and what its
/// registration says its pixels are.
#[cfg(target_os = "linux")]
pub(crate) struct DmaBufRegistrationWithItsPlaneLayout {
    pub(crate) dma_buf_fd: OwnedFd,
    pub(crate) allocation_byte_size: u64,
    pub(crate) pixel_format: PixelFormat,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) planes: crate::core::rhi::PixelPlanesWithinOneAllocation,
}

/// A looked-up registration's IOSurface and the format and extent the
/// registration states.
#[cfg(target_os = "macos")]
pub(crate) struct IOSurfaceRegistrationWithItsExtent {
    pub(crate) iosurface: objc2_core_foundation::CFRetained<objc2_io_surface::IOSurfaceRef>,
    pub(crate) pixel_format: crate::core::rhi::PixelFormat,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// A looked-up pixel buffer and what its registration says its pixels are.
#[cfg(target_os = "linux")]
pub(crate) struct PixelBufferWithItsPlaneLayout {
    pub(crate) pixel_buffer: PixelBuffer,
    pub(crate) pixel_format: PixelFormat,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) planes: crate::core::rhi::PixelPlanesWithinOneAllocation,
}

/// Rich data backing a [`SurfaceStore`], reached through the store's opaque
/// handle.
///
/// Connects to the surface-share service to exchange handles for surface ids
/// and caches what it resolves. Every surface-share IPC method (`connect`,
/// `check_in`, `check_out`, `register_texture`, …) lives here; the
/// `SurfaceStore` handle forwards each to it.
pub(crate) struct SurfaceStoreInner {
    /// Unix socket connection to the surface-share service (Linux only).
    #[cfg(target_os = "linux")]
    connection: Mutex<Option<std::os::unix::net::UnixStream>>,

    /// Mach connection to the surface-share service (macOS only).
    #[cfg(target_os = "macos")]
    connection: Mutex<Option<streamlib_surface_client::SurfaceShareMachServiceConnection>>,

    /// Local cache of checked-out surfaces (surface_id -> pixel_buffer).
    cache: Mutex<SurfaceCache>,

    /// The Unix socket path (Linux) or bootstrap service name (macOS) to
    /// connect to.
    service_name: String,

    /// Runtime ID for tracking which surfaces belong to this runtime.
    runtime_id: String,

    /// The surfaces cross-process consumers hold checked out, when a
    /// surface-share service backs this store. The pixel-buffer pool reads it
    /// to decide whether a slot may be rehanded to its producer.
    ///
    /// `None` for a store built without one: no service means no
    /// cross-process consumer can exist, and the pool's in-process refcount
    /// test alone is a complete answer.
    check_out_leases: Option<Arc<SurfaceCheckOutLeaseRegistry>>,

    /// The engine's timeline pairs by surface, shared with the Mach service
    /// that answers helpers' host-side reports against them. `None` for a
    /// store built without a service.
    #[cfg(target_os = "macos")]
    cross_process_timeline_pairs:
        Option<Arc<crate::apple::surface_share::CrossProcessTimelinePairsBySurface>>,
}

impl SurfaceStoreInner {
    /// Create a new surface store (not yet connected). Returns an
    /// `Arc<SurfaceStoreInner>` so the engine can store it directly
    /// and hand [`SurfaceStore`] handles to consumers on demand.
    pub fn new(service_name: String, runtime_id: String) -> Arc<Self> {
        Self::new_reading_check_out_leases(service_name, runtime_id, None)
    }

    /// As [`Self::new`], but reading the checkout leases of the service this
    /// store connects to. The runtime's `start()` path uses this so the
    /// pixel-buffer pool can see cross-process holders.
    pub fn new_reading_check_out_leases(
        service_name: String,
        runtime_id: String,
        check_out_leases: Option<Arc<SurfaceCheckOutLeaseRegistry>>,
    ) -> Arc<Self> {
        Self::sharing_the_services_tables(
            service_name,
            runtime_id,
            check_out_leases,
            #[cfg(target_os = "macos")]
            None,
        )
    }

    fn sharing_the_services_tables(
        service_name: String,
        runtime_id: String,
        check_out_leases: Option<Arc<SurfaceCheckOutLeaseRegistry>>,
        #[cfg(target_os = "macos")] cross_process_timeline_pairs: Option<
            Arc<crate::apple::surface_share::CrossProcessTimelinePairsBySurface>,
        >,
    ) -> Arc<Self> {
        Arc::new(SurfaceStoreInner {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            connection: Mutex::new(None),
            cache: Mutex::new(SurfaceCache::new()),
            service_name,
            runtime_id,
            check_out_leases,
            #[cfg(target_os = "macos")]
            cross_process_timeline_pairs,
        })
    }

    /// The checkout leases backing this store, if a service owns any.
    pub fn check_out_leases(&self) -> Option<&Arc<SurfaceCheckOutLeaseRegistry>> {
        self.check_out_leases.as_ref()
    }

    /// Release a single surface from the surface-share service.
    ///
    /// The `release` op is best-effort; a missing connection returns Ok since the
    /// surface-share service already treats the client's socket-close as a full
    /// release.
    pub fn release(&self, surface_id: &str) -> Result<()> {
        // Evict the local cache's strong reference first: `check_in` parks a
        // clone there, and a pixel-buffer pool frees a slot only once the
        // buffer's strong count returns to 1 — without this eviction a
        // released surface pins its pool slot for the store's lifetime.
        self.cache.lock().remove(surface_id);
        #[cfg(target_os = "linux")]
        {
            self.release_from_surface_share_unix(surface_id)
        }
        #[cfg(target_os = "macos")]
        {
            self.release_from_surface_share_mach(surface_id)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = surface_id;
            Err(Error::NotSupported(
                "SurfaceStore::release is not supported on this platform".into(),
            ))
        }
    }

    /// Disconnect from the surface-share service, dropping every surface this
    /// store resolved.
    ///
    /// Closing the connection is the release: the service treats a client's
    /// connection closing as a full release of everything it held, which is
    /// why nothing is released one id at a time here.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn disconnect(&self) -> Result<()> {
        self.cache.lock().clear();
        self.connection.lock().take();
        tracing::info!("SurfaceStore: Disconnected from the surface-share service");
        Ok(())
    }

    // =========================================================================
    // Unsupported platform stubs
    // =========================================================================

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn connect(&self) -> Result<()> {
        Err(Error::NotSupported(
            "SurfaceStore is not supported on this platform".into(),
        ))
    }

    /// `Ok` rather than the refusal its siblings return: nothing was ever
    /// connected, and a shutdown path must not fail for having nothing to do.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn disconnect(&self) -> Result<()> {
        Ok(())
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn check_in(&self, _pixel_buffer: &PixelBuffer) -> Result<String> {
        Err(Error::NotSupported(
            "SurfaceStore is not supported on this platform".into(),
        ))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn check_out(&self, _surface_id: &str) -> Result<PixelBuffer> {
        Err(Error::NotSupported(
            "SurfaceStore is not supported on this platform".into(),
        ))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn register_buffer(&self, _pool_id: &str, _pixel_buffer: &PixelBuffer) -> Result<()> {
        Err(Error::NotSupported(
            "SurfaceStore is not supported on this platform".into(),
        ))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn lookup_buffer(&self, _pool_id: &str) -> Result<PixelBuffer> {
        Err(Error::NotSupported(
            "SurfaceStore is not supported on this platform".into(),
        ))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
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

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn lookup_texture(&self, _surface_id: &str) -> Result<(crate::core::rhi::Texture, i32)> {
        Err(Error::NotSupported(
            "Texture lookup not supported on this platform".into(),
        ))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn update_image_layout(&self, _surface_id: &str, _layout: i32) -> Result<()> {
        Err(Error::NotSupported(
            "update_image_layout not supported on this platform".into(),
        ))
    }
}

unsafe impl Send for SurfaceStoreInner {}
unsafe impl Sync for SurfaceStoreInner {}

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
/// `Arc<SurfaceStoreInner>`.
pub struct SurfaceStore {
    /// Opaque handle to the host's `Arc<SurfaceStoreInner>`.
    pub(crate) handle: *const ss_c_void,
}

// SAFETY: `handle` points at an `Arc<SurfaceStoreInner>` whose
// interior is Send+Sync (Mutex-protected state, plus String fields).
unsafe impl Send for SurfaceStore {}
unsafe impl Sync for SurfaceStore {}

impl SurfaceStore {
    /// Create a new SurfaceStore handle (not yet connected), reading no
    /// service's checkout leases.
    pub fn new(service_name: String, runtime_id: String) -> Self {
        Self::from_arc_into_raw(SurfaceStoreInner::new(service_name, runtime_id))
    }

    /// As [`Self::new`], but reading the checkout leases of the service this
    /// store connects to — the shape the runtime's `start()` builds, and the
    /// one that lets the pixel-buffer pool see cross-process holders.
    pub fn new_reading_check_out_leases(
        service_name: String,
        runtime_id: String,
        check_out_leases: Arc<SurfaceCheckOutLeaseRegistry>,
    ) -> Self {
        Self::from_arc_into_raw(SurfaceStoreInner::new_reading_check_out_leases(
            service_name,
            runtime_id,
            Some(check_out_leases),
        ))
    }

    /// As [`Self::new_reading_check_out_leases`], also sharing the Mach
    /// service's timeline-pair table — the shape the runtime's `start()`
    /// builds on macOS.
    #[cfg(target_os = "macos")]
    pub fn new_sharing_the_mach_services_tables(
        service_name: String,
        runtime_id: String,
        check_out_leases: Arc<SurfaceCheckOutLeaseRegistry>,
        cross_process_timeline_pairs: Arc<
            crate::apple::surface_share::CrossProcessTimelinePairsBySurface,
        >,
    ) -> Self {
        Self::from_arc_into_raw(SurfaceStoreInner::new_sharing_the_mach_services_tables(
            service_name,
            runtime_id,
            check_out_leases,
            cross_process_timeline_pairs,
        ))
    }

    /// The checkout leases backing this store, if a service owns any.
    ///
    /// `None` also for the null-handle sentinel — nothing is checked out of a
    /// store that does not exist.
    pub fn check_out_leases(&self) -> Option<&Arc<SurfaceCheckOutLeaseRegistry>> {
        if self.is_none() {
            return None;
        }
        self.host_inner().check_out_leases()
    }

    /// The checkout lease table a pool reads to tell whether a helper holds
    /// one of its slots, if a service owns one.
    pub(crate) fn check_out_lease_registry(&self) -> Option<&SurfaceCheckOutLeaseRegistry> {
        self.check_out_leases().map(Arc::as_ref)
    }

    /// The checkout lease table of the engine's own surface store, once that
    /// store is installed and a service owns a table.
    pub(crate) fn check_out_lease_registry_of_the_engines_own(
        engines_own_surface_store: &std::sync::OnceLock<SurfaceStore>,
    ) -> Option<&SurfaceCheckOutLeaseRegistry> {
        engines_own_surface_store
            .get()
            .and_then(SurfaceStore::check_out_lease_registry)
    }

    /// Internal helper: leak an initial Arc strong count via
    /// `Arc::into_raw` and wrap it as the opaque handle.
    pub(crate) fn from_arc_into_raw(arc: Arc<SurfaceStoreInner>) -> Self {
        let handle = Arc::into_raw(arc) as *const ss_c_void;
        Self { handle }
    }

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

    /// Connect to the surface-share service (Unix
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

    /// Look up a pixel buffer with the format, extent and plane layout its
    /// registration states.
    #[cfg(target_os = "linux")]
    pub(crate) fn lookup_pixel_buffer_with_its_plane_layout(
        &self,
        surface_id: &str,
    ) -> Result<PixelBufferWithItsPlaneLayout> {
        if self.is_none() {
            return Err(Error::Configuration(
                "SurfaceStore::lookup_pixel_buffer_with_its_plane_layout: null handle".into(),
            ));
        }
        self.host_inner()
            .lookup_pixel_buffer_with_its_plane_layout(surface_id)
    }

    /// See [`SurfaceStoreInner::lookup_iosurface_registration_without_importing_it`].
    #[cfg(target_os = "macos")]
    pub(crate) fn lookup_iosurface_registration_without_importing_it(
        &self,
        surface_id: &str,
    ) -> Result<IOSurfaceRegistrationWithItsExtent> {
        if self.is_none() {
            return Err(Error::Configuration(
                "SurfaceStore::lookup_iosurface_registration_without_importing_it: null handle"
                    .into(),
            ));
        }
        self.host_inner()
            .lookup_iosurface_registration_without_importing_it(surface_id)
    }

    /// See [`SurfaceStoreInner::lookup_dma_buf_registration_without_importing_it`].
    #[cfg(target_os = "linux")]
    pub(crate) fn lookup_dma_buf_registration_without_importing_it(
        &self,
        surface_id: &str,
    ) -> Result<DmaBufRegistrationWithItsPlaneLayout> {
        if self.is_none() {
            return Err(Error::Configuration(
                "SurfaceStore::lookup_dma_buf_registration_without_importing_it: null handle"
                    .into(),
            ));
        }
        self.host_inner()
            .lookup_dma_buf_registration_without_importing_it(surface_id)
    }

    /// See [`SurfaceStoreInner::check_in_with_a_pixel_plane_layout`].
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn check_in_with_a_pixel_plane_layout(
        &self,
        pixel_buffer: &PixelBuffer,
        pixel_format: PixelFormat,
        width: u32,
        height: u32,
        planes: &crate::core::rhi::PixelPlanesWithinOneAllocation,
    ) -> Result<String> {
        self.host_inner().check_in_with_a_pixel_plane_layout(
            pixel_buffer,
            pixel_format,
            width,
            height,
            planes,
        )
    }

    /// See [`SurfaceStoreInner::check_in_a_foreign_dma_buf_fd_with_a_pixel_plane_layout`].
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn check_in_a_foreign_dma_buf_fd_with_a_pixel_plane_layout(
        &self,
        foreign_dma_buf_fd: OwnedFd,
        allocation_byte_size: u64,
        pixel_format: PixelFormat,
        width: u32,
        height: u32,
        planes: &crate::core::rhi::PixelPlanesWithinOneAllocation,
    ) -> Result<String> {
        self.host_inner()
            .check_in_a_foreign_dma_buf_fd_with_a_pixel_plane_layout(
                foreign_dma_buf_fd,
                allocation_byte_size,
                pixel_format,
                width,
                height,
                planes,
            )
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
        self.host_inner().register_texture(
            surface_id,
            texture,
            produce_done,
            consume_done,
            current_image_layout,
        )
    }

    /// **Engine-only** — register a tensor storage buffer. See
    /// [`SurfaceStoreInner::register_storage_buffer`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn host_register_storage_buffer(
        &self,
        surface_id: &str,
        buffer: &crate::core::rhi::StorageBuffer,
        tensor_layout: &crate::core::rhi::TensorStorageBufferLayout,
    ) -> Result<()> {
        if self.is_none() {
            return Err(Error::Configuration(
                "SurfaceStore::register_storage_buffer: null handle".into(),
            ));
        }
        self.host_inner()
            .register_storage_buffer(surface_id, buffer, tensor_layout)
    }

    /// **Engine-only** — register a pool slot with its cross-process timeline
    /// pair (macOS). See
    /// [`SurfaceStoreInner::register_pixel_buffer_with_timeline_pair`].
    #[cfg(target_os = "macos")]
    pub(crate) fn host_register_pixel_buffer_with_timeline_pair(
        &self,
        surface_id: &str,
        pixel_buffer: &PixelBuffer,
        timeline_pair: &Arc<crate::apple::surface_share::CrossProcessTimelinePair>,
    ) -> Result<()> {
        if self.is_none() {
            return Err(Error::Configuration(
                "SurfaceStore::register_pixel_buffer_with_timeline_pair: null handle".into(),
            ));
        }
        self.host_inner().register_pixel_buffer_with_timeline_pair(
            surface_id,
            pixel_buffer,
            timeline_pair,
        )
    }

    /// **Engine-only** — register an IOSurface-backed texture with its
    /// cross-process timeline pair (macOS). See
    /// [`SurfaceStoreInner::register_texture_with_timeline_pair`].
    #[cfg(target_os = "macos")]
    pub(crate) fn host_register_texture_with_timeline_pair(
        &self,
        surface_id: &str,
        texture: &crate::core::rhi::Texture,
        timeline_pair: &Arc<crate::apple::surface_share::CrossProcessTimelinePair>,
        current_image_layout: streamlib_consumer_rhi::VulkanLayout,
    ) -> Result<()> {
        if self.is_none() {
            return Err(Error::Configuration(
                "SurfaceStore::register_texture_with_timeline_pair: null handle".into(),
            ));
        }
        self.host_inner().register_texture_with_timeline_pair(
            surface_id,
            texture,
            timeline_pair,
            current_image_layout,
        )
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
        self.host_inner().register_pixel_buffer_with_timeline(
            surface_id,
            pixel_buffer,
            produce_done,
            consume_done,
        )
    }

    /// Look up a registered texture by surface_id.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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

    /// Update the published `VkImageLayout` for a registered texture.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(test)]
mod tests;
