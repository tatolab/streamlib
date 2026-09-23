// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Surface Store for cross-process GPU surface sharing.
//!
//! Provides check-in/check-out semantics against the per-runtime surface-share
//! service. Surfaces are cached locally after first checkout to minimize
//! round-trips.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

use crate::core::rhi::PixelBuffer;
#[cfg(target_os = "linux")]
use crate::core::rhi::PixelFormat;
use crate::core::{Error, Result};

use super::surface_check_out_lease_registry::SurfaceCheckOutLeaseRegistry;
#[cfg(target_os = "linux")]
use crate::host_rhi::HostTextureExt;

/// Every plane of a pixel buffer exported for the surface-share wire: the
/// fds to attach, the metadata arrays describing them, and the one
/// handle-type discriminator the checkout importer dispatches on.
///
/// The plane fds are owned, never observed: an exporter mints a fresh fd per
/// call and surrenders it, so adoption here is what closes each one exactly
/// once — on the send, on a refusal, and on every early return in between.
#[cfg(target_os = "linux")]
struct ExportedPlaneWireHandles {
    plane_fds: Vec<OwnedFd>,
    plane_sizes: Vec<u64>,
    plane_offsets: Vec<u64>,
    handle_type: &'static str,
}

/// Export every plane of `pixel_buffer` for a surface-share registration.
///
/// Single-plane pixel buffers return a one-element vec; multi-plane
/// DMA-BUFs (e.g. NV12 under DRM format modifiers) one fd per plane;
/// OPAQUE_FD-flavored buffers (CUDA targets) a single OPAQUE_FD handle.
#[cfg(target_os = "linux")]
fn exported_plane_wire_handles(pixel_buffer: &PixelBuffer) -> Result<ExportedPlaneWireHandles> {
    use crate::core::rhi::RhiPixelBufferExport;

    adopt_exported_planes_into_wire_handles(pixel_buffer.export_plane_handles()?)
}

/// Adopt one export's worth of plane handles into the wire shape.
///
/// One registration carries one flavour — a buffer whose planes export under
/// two is refused rather than published under whichever flavour happened to
/// come last, which would import the other planes through the wrong Vulkan
/// external-handle type.
///
/// Every `exported_planes` fd must come from an export that minted it for
/// this process: they are adopted, not borrowed. `RhiExternalHandle` also
/// carries kernel-delivered fds on the import side (see
/// [`external_plane_handles_for_flavour`]), and those must never reach here.
#[cfg(target_os = "linux")]
fn adopt_exported_planes_into_wire_handles(
    exported_planes: Vec<crate::core::rhi::RhiExternalHandle>,
) -> Result<ExportedPlaneWireHandles> {
    use crate::core::rhi::RhiExternalHandle;

    let mut plane_fds: Vec<OwnedFd> = Vec::with_capacity(exported_planes.len());
    let mut plane_sizes: Vec<u64> = Vec::with_capacity(exported_planes.len());
    let mut plane_offsets: Vec<u64> = Vec::with_capacity(exported_planes.len());
    let mut plane_flavours: Vec<&'static str> = Vec::with_capacity(exported_planes.len());
    for exported_plane in exported_planes {
        let (exported_plane_fd, size, this_plane_flavour) = match exported_plane {
            RhiExternalHandle::DmaBuf { fd, size } => (fd, size, SURFACE_HANDLE_TYPE_DMA_BUF),
            RhiExternalHandle::OpaqueFd { fd, size } => (fd, size, SURFACE_HANDLE_TYPE_OPAQUE_FD),
        };
        // SAFETY: the export minted this fd for us and handed it to no one
        // else.
        plane_fds.push(unsafe { OwnedFd::from_raw_fd(exported_plane_fd) });
        plane_sizes.push(size as u64);
        plane_offsets.push(0);
        plane_flavours.push(this_plane_flavour);
    }
    // Validated after every fd is adopted, so the refusal closes all of them
    // rather than only the ones the loop reached.
    let handle_type = plane_flavours
        .first()
        .copied()
        .unwrap_or(SURFACE_HANDLE_TYPE_DMA_BUF);
    if plane_flavours
        .iter()
        .any(|plane_flavour| *plane_flavour != handle_type)
    {
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

/// Export one timeline-semaphore edge as the OPAQUE_FD the wire carries.
///
/// The host keeps the semaphore object; the kernel duplicates the fd during
/// SCM_RIGHTS, so this copy is ours alone and closes with the scope that
/// adopted it.
#[cfg(target_os = "linux")]
fn exported_timeline_edge_opaque_fd(
    operation: &str,
    edge_name: &str,
    timeline: &crate::vulkan::rhi::HostVulkanTimelineSemaphore,
) -> Result<OwnedFd> {
    let exported_fd = timeline.export_opaque_fd().map_err(|export_failure| {
        Error::Configuration(format!(
            "{operation}: failed to export the {edge_name} timeline opaque fd: {export_failure}"
        ))
    })?;
    // SAFETY: each export mints a fresh fd this process owns and has handed
    // to no one else.
    Ok(unsafe { OwnedFd::from_raw_fd(exported_fd) })
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

/// Wire value of `resource_type` for a texture registration — the only
/// kind a texture lookup imports.
#[cfg(target_os = "linux")]
const SURFACE_RESOURCE_TYPE_TEXTURE: &str = "texture";

/// Wire value of `resource_type` for a pixel-buffer registration.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const SURFACE_RESOURCE_TYPE_PIXEL_BUFFER: &str = "pixel_buffer";

/// How long a connect waits for the service to admit this process.
#[cfg(target_os = "macos")]
const SURFACE_SHARE_MACH_CONNECT_HANDSHAKE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(10);

/// A fresh send right to the IOSurface `pixel_buffer`'s memory is, for a
/// registration to move to the service.
#[cfg(target_os = "macos")]
fn exported_iosurface_port(
    pixel_buffer: &PixelBuffer,
) -> Result<streamlib_surface_client::OwnedMachSendRight> {
    pixel_buffer
        .buffer_ref()
        .inner
        .export_iosurface_mach_send_right()
}

/// Reply flag announcing a `produce_done` timeline edge appended after the
/// plane fds.
#[cfg(target_os = "linux")]
const SURFACE_REPLY_HAS_PRODUCE_DONE_FD: &str = "has_produce_done_fd";

/// Reply flag announcing a `consume_done` timeline edge appended after the
/// plane fds (and after `produce_done` when both are present).
#[cfg(target_os = "linux")]
const SURFACE_REPLY_HAS_CONSUME_DONE_FD: &str = "has_consume_done_fd";

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

/// Adopt the fds a surface-share reply delivered over SCM_RIGHTS.
///
/// Each one landed in this process's table as a fresh descriptor that no
/// other party closes, so adopting them the moment they arrive is what
/// makes every exit of a lookup — a wire error, a refusal, a failed
/// import — close them exactly once.
#[cfg(target_os = "linux")]
fn adopt_reply_fds(received_fds: Vec<std::os::fd::RawFd>) -> Vec<OwnedFd> {
    received_fds
        .into_iter()
        // SAFETY: the kernel delivered these fds to this process with the
        // reply and nothing else holds them.
        .map(|fd| unsafe { OwnedFd::from_raw_fd(fd) })
        .collect()
}

/// The plane fds of a lookup reply, with the timeline edges the service
/// appends after them closed.
///
/// The service announces each appended edge with a flag; a host-side
/// lookup imports planes only. A reply carrying fewer fds than its flags
/// promise is refused rather than read as a shorter plane list.
#[cfg(target_os = "linux")]
fn plane_fds_of_reply(
    operation: &str,
    response: &serde_json::Value,
    mut reply_fds: Vec<OwnedFd>,
) -> Result<Vec<OwnedFd>> {
    let appended_edges = [
        SURFACE_REPLY_HAS_PRODUCE_DONE_FD,
        SURFACE_REPLY_HAS_CONSUME_DONE_FD,
    ]
    .iter()
    .filter(|flag| response.get(*flag).and_then(serde_json::Value::as_bool) == Some(true))
    .count();
    if reply_fds.len() < appended_edges + 1 {
        return Err(Error::Configuration(format!(
            "{operation}: the reply carried {} fds, fewer than the {} its flags promise",
            reply_fds.len(),
            appended_edges + 1
        )));
    }
    reply_fds.truncate(reply_fds.len() - appended_edges);
    Ok(reply_fds)
}

/// The `plane_sizes` a lookup reply states, one per plane fd; zeros when
/// the reply states none or a different count.
#[cfg(target_os = "linux")]
fn plane_sizes_of_reply(response: &serde_json::Value, plane_count: usize) -> Vec<u64> {
    response
        .get("plane_sizes")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_u64()).collect())
        .filter(|v: &Vec<u64>| v.len() == plane_count)
        .unwrap_or_else(|| vec![0u64; plane_count])
}

/// Release the plane fds to the pixel-buffer import about to take them,
/// which owns each one from here: handed to the driver at its import, or
/// closed before that.
#[cfg(target_os = "linux")]
fn leave_plane_fds_to_the_import(plane_fds: Vec<OwnedFd>) {
    use std::os::fd::IntoRawFd as _;
    for plane_fd in plane_fds {
        let _ = plane_fd.into_raw_fd();
    }
}

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

/// The wire spelling of a device UUID: 32 lowercase hex characters.
#[cfg(target_os = "linux")]
fn lowercase_hex_of_device_uuid(device_uuid: [u8; 16]) -> String {
    use std::fmt::Write;
    device_uuid
        .iter()
        .fold(String::with_capacity(32), |mut lowercase_hex, byte| {
            let _ = write!(lowercase_hex, "{byte:02x}");
            lowercase_hex
        })
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

    /// The cached buffer for `surface_id`, counted as one more checkout.
    fn checked_out_clone(&mut self, surface_id: &str) -> Option<PixelBuffer> {
        let cached = self.surfaces.get_mut(surface_id)?;
        cached.checkout_count += 1;
        tracing::trace!(
            "SurfaceStore: Cache hit for '{}' (checkout #{})",
            surface_id,
            cached.checkout_count
        );
        Some(cached.pixel_buffer.clone())
    }

    /// The cached buffer for `surface_id`, uncounted.
    fn cached_clone(&self, surface_id: &str) -> Option<PixelBuffer> {
        self.surfaces
            .get(surface_id)
            .map(|cached| cached.pixel_buffer.clone())
    }

    fn clear(&mut self) {
        self.surfaces.clear();
    }
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

    /// As [`Self::new_reading_check_out_leases`], also sharing the Mach
    /// service's timeline-pair table so a registration with a pair can be
    /// ordered host-side when a helper cannot import it.
    #[cfg(target_os = "macos")]
    pub fn new_sharing_the_mach_services_tables(
        service_name: String,
        runtime_id: String,
        check_out_leases: Arc<SurfaceCheckOutLeaseRegistry>,
        cross_process_timeline_pairs: Arc<
            crate::apple::surface_share::CrossProcessTimelinePairsBySurface,
        >,
    ) -> Arc<Self> {
        Self::sharing_the_services_tables(
            service_name,
            runtime_id,
            Some(check_out_leases),
            Some(cross_process_timeline_pairs),
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

    /// Check in a pixel buffer via Unix socket, returning a surface ID.
    #[cfg(target_os = "linux")]
    pub fn check_in(&self, pixel_buffer: &PixelBuffer) -> Result<String> {
        let exported_planes = exported_plane_wire_handles(pixel_buffer)?;

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

        let response = self.send_surface_share_request_owning_fds(
            "check_in",
            &request,
            exported_planes.plane_fds,
        )?;

        let surface_id = surface_id_of_check_in_answer(&response)?;

        self.cache
            .lock()
            .insert(surface_id.clone(), pixel_buffer.clone());

        tracing::debug!("SurfaceStore: Checked in as '{}'", surface_id);

        Ok(surface_id)
    }

    /// Check out a surface by ID via Unix socket.
    #[cfg(target_os = "linux")]
    pub fn check_out(&self, surface_id: &str) -> Result<PixelBuffer> {
        if let Some(cached) = self.cache.lock().checked_out_clone(surface_id) {
            return Ok(cached);
        }

        // Cache miss - fetch from the surface-share service
        tracing::debug!(
            "SurfaceStore: Cache miss for '{}', fetching from the surface-share service",
            surface_id
        );

        // `lookup`, not `check_out`: this store is the service owner's own
        // process, and the cached `PixelBuffer` clone it takes below IS its
        // protection — the pool's in-process refcount. A `check_out` here
        // would mint a lease on the host's own long-lived connection that
        // nothing ever releases (the host's `release` op unregisters, it
        // does not unlease), pinning the slot for the runtime's life.
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
            streamlib_surface_client::MAX_SCM_RIGHTS_FDS,
        )
        .map_err(|e| Error::Configuration(format!("Unix socket check_out failed: {}", e)))?;
        let reply_fds = adopt_reply_fds(received_fds);

        if let Some(error) = response
            .get("error")
            .and_then(|v: &serde_json::Value| v.as_str())
        {
            return Err(Error::Configuration(format!("check_out: {}", error)));
        }
        let plane_fds = plane_fds_of_reply("check_out", &response, reply_fds)?;

        // Import every plane under the flavour the service says it registered.
        // The two flavours are different Vulkan external-handle types, so
        // importing an OPAQUE_FD fd as DMA-BUF is a driver-level wrong-import
        // rather than a clean failure.
        use crate::core::rhi::{PixelFormat, RhiPixelBufferImport};

        let plane_sizes = plane_sizes_of_reply(&response, plane_fds.len());
        let raw_plane_fds: Vec<std::os::fd::RawFd> =
            plane_fds.iter().map(OwnedFd::as_raw_fd).collect();
        let handles = external_plane_handles_for_flavour(
            response
                .get("handle_type")
                .and_then(|v| v.as_str())
                .unwrap_or(SURFACE_HANDLE_TYPE_DMA_BUF),
            &raw_plane_fds,
            &plane_sizes,
        )?;
        leave_plane_fds_to_the_import(plane_fds);
        let pixel_buffer =
            PixelBuffer::from_external_plane_handles(handles, 0, 0, PixelFormat::default())?;

        // Cache for future use
        self.cache
            .lock()
            .insert(surface_id.to_string(), pixel_buffer.clone());

        Ok(pixel_buffer)
    }

    /// Send one request over the surface-share socket, passing `fds` with
    /// SCM_RIGHTS under `operation`'s name, and return the service's reply.
    ///
    /// Takes the fds by value because the ordering here is the contract:
    /// the raw descriptors must stay valid across the send, this side's
    /// copies must close before the send's outcome is inspected (the
    /// service dup'd what it needs during the send), and any fd the reply
    /// carries must close on the success path. Every caller goes through
    /// here so those four steps cannot drift apart.
    #[cfg(target_os = "linux")]
    fn send_surface_share_request_owning_fds(
        &self,
        operation: &str,
        request: &serde_json::Value,
        fds: Vec<OwnedFd>,
    ) -> Result<serde_json::Value> {
        let connection = self.connection.lock();
        let stream = connection.as_ref().ok_or_else(|| {
            Error::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        let raw_fds: Vec<std::os::unix::io::RawFd> = fds.iter().map(OwnedFd::as_raw_fd).collect();
        let send_result =
            streamlib_surface_client::send_request_with_fds(stream, request, &raw_fds, 0);
        drop(fds);
        let (response, response_fds) = send_result.map_err(|failure| {
            Error::Configuration(format!("Unix socket {operation} failed: {failure}"))
        })?;
        // Neither op returns fds; close any the service attached so a future
        // protocol drift doesn't leak them.
        for fd in &response_fds {
            unsafe { libc::close(*fd) };
        }
        Ok(response)
    }

    /// Send one `register` request and hold the service to its answer.
    #[cfg(target_os = "linux")]
    fn send_surface_share_registration(
        &self,
        operation: &str,
        request: &serde_json::Value,
        fds: Vec<OwnedFd>,
    ) -> Result<()> {
        let response = self.send_surface_share_request_owning_fds(operation, request, fds)?;
        refusal_of_a_registration_answer(operation, &response)
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
            "resource_type": SURFACE_RESOURCE_TYPE_PIXEL_BUFFER,
            "handle_type": exported_planes.handle_type,
            "plane_sizes": exported_planes.plane_sizes,
            "plane_offsets": exported_planes.plane_offsets,
        });

        self.send_surface_share_registration("register", &request, exported_planes.plane_fds)?;
        tracing::debug!("SurfaceStore: Registered buffer '{}'", pool_id);
        Ok(())
    }

    /// Register a surface-export staging buffer and the timeline its
    /// refills signal, so a helper process can check the pair out and
    /// reach the staging — importing it into an external device API, or
    /// mapping it, according to the residency it was minted at.
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
    pub fn register_surface_export_staging(
        &self,
        surface_id: &str,
        staging_buffer: &crate::vulkan::rhi::HostVulkanBuffer,
        staging_byte_size: u64,
        width: u32,
        height: u32,
        format: PixelFormat,
        refill_done: &crate::vulkan::rhi::HostVulkanTimelineSemaphore,
    ) -> Result<()> {
        // Read ahead of the export so a staging with no allocation to
        // state fails before an fd is minted. A conforming OPAQUE_FD
        // import binds the exporter's memory type index
        // (VUID-VkMemoryAllocateInfo-allocationSize-01742) and OPAQUE_FD
        // has no fd-properties query to derive it from, so the consumer
        // has nowhere else to get it.
        let memory_type_index = memory_type_index_stated_by_an_opaque_fd_export(
            staging_buffer.vma_allocation_memory_type_index(),
            surface_id,
            "surface-export staging",
        )?;
        let exported_staging_fd = staging_buffer.export_opaque_fd_memory()?;
        // SAFETY: each export mints a fresh fd this process owns and has
        // handed to no one; adopting it here is what closes it exactly once
        // — including on the early return the timeline export can take.
        let staging_fd = unsafe { OwnedFd::from_raw_fd(exported_staging_fd) };
        let refill_done_fd = exported_timeline_edge_opaque_fd(
            "register_surface_export_staging",
            "refill_done",
            refill_done,
        )?;

        let request = surface_export_staging_registration_payload(
            surface_id,
            &self.runtime_id,
            staging_byte_size,
            width,
            height,
            format,
            memory_type_index,
        );

        self.send_surface_share_registration(
            "register_surface_export_staging",
            &request,
            vec![staging_fd, refill_done_fd],
        )?;
        tracing::debug!(
            "SurfaceStore: Registered surface-export staging '{}' ({} bytes)",
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
        let exported_memory_fd = if is_opaque_fd {
            texture.vulkan_inner().export_opaque_fd_memory()?
        } else {
            texture.vulkan_inner().export_dma_buf_fd()?
        };
        // SAFETY: each export mints a fresh fd this process owns and has
        // handed to no one else; adopting it here is what closes it exactly
        // once, on every path out — including the early returns the timeline
        // exports below can take.
        let owned_memory_fd = unsafe { OwnedFd::from_raw_fd(exported_memory_fd) };

        // Optionally export the producer-side `produce_done` and
        // consumer-side `consume_done` timeline-semaphores as
        // OPAQUE_FDs (single-writer-per-edge model — see
        // `docs/architecture/adapter-timeline-single-writer.md`).
        let produce_done_fd = produce_done
            .map(|edge| exported_timeline_edge_opaque_fd("register_texture", "produce_done", edge))
            .transpose()?;
        let consume_done_fd = consume_done
            .map(|edge| exported_timeline_edge_opaque_fd("register_texture", "consume_done", edge))
            .transpose()?;

        // Per-flavor wire fields. The DMA-BUF path carries the DRM
        // modifier + per-plane layout the EGL / Vulkan import paths
        // need. The OPAQUE_FD path carries the `vk_image_*` round-trip
        // shape `cudaExternalMemoryGetMappedMipmappedArray` requires —
        // matches the fixed shape `HostVulkanTexture::new_opaque_fd_export`
        // hardcodes (2D, mipLevels=1, arrayLayers=1, samples=1,
        // tiling=OPTIMAL, usage=TRANSFER_SRC|TRANSFER_DST|SAMPLED|STORAGE).
        let request = if is_opaque_fd {
            let allocation_size = texture.vulkan_inner().vma_allocation_size() as u64;
            // Raw-handle export contract fields. Both exist by
            // construction here: the OPAQUE_FD memory export above
            // already proved the allocation and its owning device.
            let memory_type_index = memory_type_index_stated_by_an_opaque_fd_export(
                texture.vulkan_inner().vma_allocation_memory_type_index(),
                surface_id,
                "OPAQUE_FD texture registration",
            )?;
            let exporting_device_uuid = lowercase_hex_of_device_uuid(
                texture
                    .vulkan_inner()
                    .exporting_physical_device_uuid()
                    .ok_or_else(|| {
                        Error::GpuError(format!(
                            "OPAQUE_FD texture registration for {surface_id:?} has no stored \
                             device to read the exporting device UUID from; an import on the \
                             wrong GPU corrupts silently instead of failing"
                        ))
                    })?,
            );
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
                "format": texture.format().wire_name(),
                "resource_type": SURFACE_RESOURCE_TYPE_TEXTURE,
                "handle_type": SURFACE_HANDLE_TYPE_OPAQUE_FD,
                "plane_sizes": [allocation_size],
                "plane_offsets": [0u64],
                "plane_strides": [0u64],
                "drm_format_modifier": 0u64,
                SURFACE_REPLY_HAS_PRODUCE_DONE_FD: produce_done_fd.is_some(),
                SURFACE_REPLY_HAS_CONSUME_DONE_FD: consume_done_fd.is_some(),
                "current_image_layout": current_image_layout.as_vk().as_raw(),
                "vk_image_type": VK_IMAGE_TYPE_2D,
                "vk_image_mip_levels": 1u32,
                "vk_image_array_layers": 1u32,
                "vk_image_samples": VK_SAMPLE_COUNT_1,
                "vk_image_tiling": VK_IMAGE_TILING_OPTIMAL,
                "vk_image_usage": vk_image_usage,
                "vk_image_allocation_size": allocation_size,
                "vk_memory_type_index": memory_type_index,
                "exporting_device_uuid": exporting_device_uuid,
            })
        } else {
            dma_buf_texture_registration_payload(
                texture.vulkan_inner(),
                surface_id,
                &self.runtime_id,
                produce_done_fd.is_some(),
                consume_done_fd.is_some(),
                current_image_layout,
            )
        };

        let mut wire_fds_in_published_order: Vec<OwnedFd> = vec![owned_memory_fd];
        wire_fds_in_published_order.extend(produce_done_fd);
        wire_fds_in_published_order.extend(consume_done_fd);
        self.send_surface_share_registration(
            "register_texture",
            &request,
            wire_fds_in_published_order,
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

        // Export `produce_done` + `consume_done` as OPAQUE_FDs (the
        // single-writer-per-edge pair documented in
        // `docs/architecture/adapter-timeline-single-writer.md`). The
        // surface-share daemon's wire format peels both trailing FDs
        // in the published order.
        let produce_done_fd = produce_done
            .map(|edge| {
                exported_timeline_edge_opaque_fd(
                    "register_pixel_buffer_with_timeline",
                    "produce_done",
                    edge,
                )
            })
            .transpose()?;
        let consume_done_fd = consume_done
            .map(|edge| {
                exported_timeline_edge_opaque_fd(
                    "register_pixel_buffer_with_timeline",
                    "consume_done",
                    edge,
                )
            })
            .transpose()?;

        let request = serde_json::json!({
            "op": "register",
            "surface_id": surface_id,
            "runtime_id": self.runtime_id,
            "width": pixel_buffer.width,
            "height": pixel_buffer.height,
            "format": pixel_buffer.format().wire_name(),
            "resource_type": SURFACE_RESOURCE_TYPE_PIXEL_BUFFER,
            "handle_type": exported_planes.handle_type,
            "plane_sizes": exported_planes.plane_sizes,
            "plane_offsets": exported_planes.plane_offsets,
            SURFACE_REPLY_HAS_PRODUCE_DONE_FD: produce_done_fd.is_some(),
            SURFACE_REPLY_HAS_CONSUME_DONE_FD: consume_done_fd.is_some(),
        });

        let mut wire_fds_in_published_order = exported_planes.plane_fds;
        wire_fds_in_published_order.extend(produce_done_fd);
        wire_fds_in_published_order.extend(consume_done_fd);
        self.send_surface_share_registration(
            "register_pixel_buffer_with_timeline",
            &request,
            wire_fds_in_published_order,
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
        if let Some(cached) = self.cache.lock().cached_clone(pool_id) {
            return Ok(cached);
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
            streamlib_surface_client::MAX_SCM_RIGHTS_FDS,
        )
        .map_err(|e| Error::Configuration(format!("Unix socket lookup failed: {}", e)))?;
        let reply_fds = adopt_reply_fds(received_fds);

        if let Some(error) = response
            .get("error")
            .and_then(|v: &serde_json::Value| v.as_str())
        {
            return Err(Error::Configuration(format!("lookup: {}", error)));
        }
        let plane_fds = plane_fds_of_reply("lookup", &response, reply_fds)?;

        // Dispatch on the wire-level handle type. OPAQUE_FD lookups can't
        // construct a host-side `PixelBuffer` (that import path is
        // DMA-BUF-only — see `RhiPixelBufferImport::from_external_plane_handles`).
        // Subprocess consumers go through `streamlib-surface-client` directly
        // and import via `streamlib_consumer_rhi::ConsumerVulkanBuffer::from_opaque_fd`.
        let handle_type = response
            .get("handle_type")
            .and_then(|v| v.as_str())
            .unwrap_or(SURFACE_HANDLE_TYPE_DMA_BUF);

        if handle_type == SURFACE_HANDLE_TYPE_OPAQUE_FD {
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

        let plane_sizes = plane_sizes_of_reply(&response, plane_fds.len());
        let handles: Vec<RhiExternalHandle> = plane_fds
            .iter()
            .zip(plane_sizes.iter())
            .map(|(fd, size)| RhiExternalHandle::DmaBuf {
                fd: fd.as_raw_fd(),
                size: *size as usize,
            })
            .collect();
        leave_plane_fds_to_the_import(plane_fds);
        PixelBuffer::from_external_plane_handles(handles, 0, 0, PixelFormat::default())
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
            streamlib_surface_client::MAX_SCM_RIGHTS_FDS,
        )
        .map_err(|e| Error::Configuration(format!("Unix socket lookup_texture failed: {}", e)))?;
        let reply_fds = adopt_reply_fds(received_fds);

        if let Some(error) = response
            .get("error")
            .and_then(|v: &serde_json::Value| v.as_str())
        {
            return Err(Error::Configuration(format!("lookup_texture: {}", error)));
        }

        // Refused by name before anything is parsed: a pool slot answers
        // this lookup too, in the pixel-buffer vocabulary, and it is not a
        // texture whatever its format string parses as.
        let resource_type = response
            .get("resource_type")
            .and_then(|v| v.as_str())
            .unwrap_or(SURFACE_RESOURCE_TYPE_PIXEL_BUFFER);
        if resource_type != SURFACE_RESOURCE_TYPE_TEXTURE {
            return Err(Error::Configuration(format!(
                "lookup_texture: {surface_id:?} is registered as a {resource_type}, not a texture"
            )));
        }

        // A texture registration carries one plane; anything past it closes
        // here.
        let mut plane_fds = plane_fds_of_reply("lookup_texture", &response, reply_fds)?.into_iter();
        let dma_buf_fd = plane_fds
            .next()
            .expect("plane_fds_of_reply refuses an empty plane list");
        drop(plane_fds);

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

        let format = TextureFormat::from_wire_name(format_str).ok_or_else(|| {
            Error::Configuration(format!("lookup_texture: unknown format '{}'", format_str))
        })?;

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
    // macOS: raw Mach client
    // =========================================================================

    /// Connect to the surface-share service's bootstrap name.
    #[cfg(target_os = "macos")]
    pub fn connect(&self) -> Result<()> {
        let connection = streamlib_surface_client::SurfaceShareMachServiceConnection::connect(
            &self.service_name,
            SURFACE_SHARE_MACH_CONNECT_HANDSHAKE_TIMEOUT,
        )
        .map_err(|e| {
            Error::Configuration(format!(
                "Failed to connect to surface-share Mach service '{}': {}",
                self.service_name, e
            ))
        })?;
        *self.connection.lock() = Some(connection);
        tracing::info!(
            "SurfaceStore: Connected to surface-share Mach service '{}'",
            self.service_name
        );
        Ok(())
    }

    /// Check in a pixel buffer's IOSurface, returning the id the service
    /// minted for it.
    #[cfg(target_os = "macos")]
    pub fn check_in(&self, pixel_buffer: &PixelBuffer) -> Result<String> {
        let request = serde_json::json!({
            "op": "check_in",
            "runtime_id": self.runtime_id,
            "width": pixel_buffer.width,
            "height": pixel_buffer.height,
            "format": pixel_buffer.format().wire_name(),
            "resource_type": SURFACE_RESOURCE_TYPE_PIXEL_BUFFER,
        });
        let (response, _) = self.send_surface_share_mach_request(
            "check_in",
            &request,
            vec![exported_iosurface_port(pixel_buffer)?],
        )?;
        let surface_id = surface_id_of_check_in_answer(&response)?;
        self.cache
            .lock()
            .insert(surface_id.clone(), pixel_buffer.clone());
        tracing::debug!("SurfaceStore: Checked in as '{}'", surface_id);
        Ok(surface_id)
    }

    /// Resolve a surface id to a pixel buffer over its IOSurface, caching it.
    ///
    /// `lookup`, not `check_out`, for the same reason as on Linux: this store
    /// is the service owner's own process, and the cached clone is its claim.
    #[cfg(target_os = "macos")]
    pub fn check_out(&self, surface_id: &str) -> Result<PixelBuffer> {
        if let Some(cached) = self.cache.lock().checked_out_clone(surface_id) {
            return Ok(cached);
        }
        let pixel_buffer = self.import_looked_up_iosurface("check_out", surface_id)?;
        self.cache
            .lock()
            .insert(surface_id.to_string(), pixel_buffer.clone());
        Ok(pixel_buffer)
    }

    /// Register a pool slot's IOSurface under `pool_id`.
    #[cfg(target_os = "macos")]
    pub fn register_buffer(&self, pool_id: &str, pixel_buffer: &PixelBuffer) -> Result<()> {
        self.send_pixel_buffer_registration(pool_id, pixel_buffer, None)?;
        tracing::debug!("SurfaceStore: Registered buffer '{}'", pool_id);
        Ok(())
    }

    /// Register a pool slot's IOSurface under `surface_id` with its timeline
    /// pair. The pair's shared events cross beside the surface; when either
    /// will not export, the registration crosses without them and the pair
    /// orders host-side.
    #[cfg(target_os = "macos")]
    pub fn register_pixel_buffer_with_timeline_pair(
        &self,
        surface_id: &str,
        pixel_buffer: &PixelBuffer,
        timeline_pair: &Arc<crate::apple::surface_share::CrossProcessTimelinePair>,
    ) -> Result<()> {
        let cross_process_timeline_pairs =
            self.cross_process_timeline_pairs.as_ref().ok_or_else(|| {
                Error::Configuration(format!(
                    "register_pixel_buffer_with_timeline_pair('{surface_id}'): this store shares \
                     no timeline-pair table with a surface-share service"
                ))
            })?;
        // Recorded only once the service accepted the id: a refused duplicate
        // must not displace the live registration's pair.
        self.send_pixel_buffer_registration(surface_id, pixel_buffer, Some(timeline_pair))?;
        cross_process_timeline_pairs.insert(surface_id, Arc::clone(timeline_pair));
        tracing::debug!(
            "SurfaceStore: Registered buffer '{}' with its timeline pair (host-side ordering: {})",
            surface_id,
            timeline_pair.orders_host_side()
        );
        Ok(())
    }

    /// Send one `register` for a pool slot's IOSurface, with its timeline
    /// pair's shared events after it when there is a pair and it exports.
    #[cfg(target_os = "macos")]
    fn send_pixel_buffer_registration(
        &self,
        surface_id: &str,
        pixel_buffer: &PixelBuffer,
        timeline_pair: Option<&crate::apple::surface_share::CrossProcessTimelinePair>,
    ) -> Result<()> {
        let mut ports = vec![exported_iosurface_port(pixel_buffer)?];
        let carries_timeline_pair = timeline_pair
            .is_some_and(|timeline_pair| timeline_pair.append_exported_send_rights_to(&mut ports));
        let request = serde_json::json!({
            "op": "register",
            "surface_id": surface_id,
            "runtime_id": self.runtime_id,
            "width": pixel_buffer.width,
            "height": pixel_buffer.height,
            "format": pixel_buffer.format().wire_name(),
            "resource_type": SURFACE_RESOURCE_TYPE_PIXEL_BUFFER,
            streamlib_surface_client::SURFACE_SHARE_HAS_PRODUCE_DONE_PORT: carries_timeline_pair,
            streamlib_surface_client::SURFACE_SHARE_HAS_CONSUME_DONE_PORT: carries_timeline_pair,
        });
        let (response, _) = self.send_surface_share_mach_request("register", &request, ports)?;
        refusal_of_a_registration_answer("register", &response)
    }

    /// Resolve a registered pool slot to a pixel buffer over its IOSurface.
    #[cfg(target_os = "macos")]
    pub fn lookup_buffer(&self, pool_id: &str) -> Result<PixelBuffer> {
        if let Some(cached) = self.cache.lock().cached_clone(pool_id) {
            return Ok(cached);
        }
        self.import_looked_up_iosurface("lookup", pool_id)
    }

    /// `lookup` `surface_id` and import the IOSurface the answer's port
    /// names, zero-copy.
    #[cfg(target_os = "macos")]
    fn import_looked_up_iosurface(&self, operation: &str, surface_id: &str) -> Result<PixelBuffer> {
        use crate::core::rhi::{PixelFormat, RhiExternalHandle, RhiPixelBufferImport};

        let request = serde_json::json!({"op": "lookup", "surface_id": surface_id});
        let (answer, reply_ports) =
            self.send_surface_share_mach_request(operation, &request, Vec::new())?;
        // Any timeline ports after the IOSurface's are the engine's own
        // timelines, which this process already holds; they are released here.
        let Some(iosurface_port) = reply_ports.into_iter().next() else {
            return Err(Error::Configuration(format!(
                "{operation}: the answer for '{surface_id}' carried no IOSurface port"
            )));
        };
        let stated_u32 = |key: &str| {
            answer
                .get(key)
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(0)
        };
        let format = answer
            .get("format")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("the answer for '{surface_id}' names no format"))
            .and_then(PixelFormat::parse_wire_name)
            .map_err(|unreadable| Error::Configuration(format!("{operation}: {unreadable}")))?;
        PixelBuffer::from_external_handle(
            RhiExternalHandle::IOSurfaceMachPort {
                port: iosurface_port.into_raw_name(),
            },
            stated_u32("width"),
            stated_u32("height"),
            format,
        )
    }

    /// Send one request under `operation`'s name and hold the service to its
    /// answer, returning it with the ports it carries.
    #[cfg(target_os = "macos")]
    fn send_surface_share_mach_request(
        &self,
        operation: &str,
        request: &serde_json::Value,
        ports: Vec<streamlib_surface_client::OwnedMachSendRight>,
    ) -> Result<(
        serde_json::Value,
        Vec<streamlib_surface_client::OwnedMachSendRight>,
    )> {
        let connection = self.connection.lock();
        let connection = connection.as_ref().ok_or_else(|| {
            Error::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;
        let (response, reply_ports) =
            connection
                .send_request_with_ports(request, ports)
                .map_err(|failure| {
                    Error::Configuration(format!(
                        "Mach surface-share {operation} failed: {failure}"
                    ))
                })?;
        if let Some(error) = response.get("error").and_then(serde_json::Value::as_str) {
            return Err(Error::Configuration(format!("{operation}: {error}")));
        }
        Ok((response, reply_ports))
    }

    /// Best-effort `release`; with no connection there is nothing to release,
    /// because the service already let go when the connection closed.
    #[cfg(target_os = "macos")]
    fn release_from_surface_share_mach(&self, surface_id: &str) -> Result<()> {
        let request = serde_json::json!({
            "op": "release",
            "surface_id": surface_id,
            "runtime_id": self.runtime_id,
        });
        if let Some(connection) = self.connection.lock().as_ref() {
            let _ = connection.send_request_with_ports(&request, Vec::new());
        }
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

/// The memory type index an OPAQUE_FD registration must publish, or a
/// refusal naming what had no allocation to read one from.
///
/// One rule for every OPAQUE_FD export: the handle type has no
/// `vkGetMemoryFdPropertiesKHR` query, so an importer that is not told
/// the index guesses, and a guess that happens to bind is the two sides
/// coincidentally landing on the same memory type.
#[cfg(target_os = "linux")]
fn memory_type_index_stated_by_an_opaque_fd_export(
    vma_allocation_memory_type_index: Option<u32>,
    surface_id: &str,
    exported_resource_description: &'static str,
) -> Result<u32> {
    vma_allocation_memory_type_index.ok_or_else(|| {
        Error::GpuError(format!(
            "{exported_resource_description} for {surface_id:?} has no VMA allocation to \
             read a memory type index from; a consumer import without one binds the wrong \
             memory type instead of failing"
        ))
    })
}

/// The surface-share registration payload for a CPU-readback export
/// staging.
///
/// `memory_type_index` comes from
/// [`memory_type_index_stated_by_an_opaque_fd_export`], which is where
/// the reason it is mandatory is written down.
#[cfg(target_os = "linux")]
fn surface_export_staging_registration_payload(
    surface_id: &str,
    runtime_id: &str,
    staging_byte_size: u64,
    width: u32,
    height: u32,
    format: PixelFormat,
    memory_type_index: u32,
) -> serde_json::Value {
    serde_json::json!({
        "op": "register",
        "surface_id": surface_id,
        "runtime_id": runtime_id,
        "width": width,
        "height": height,
        "format": format.wire_name(),
        "resource_type": SURFACE_RESOURCE_TYPE_PIXEL_BUFFER,
        "handle_type": SURFACE_HANDLE_TYPE_OPAQUE_FD,
        "plane_sizes": [staging_byte_size],
        "plane_offsets": [0],
        SURFACE_REPLY_HAS_PRODUCE_DONE_FD: true,
        SURFACE_REPLY_HAS_CONSUME_DONE_FD: false,
        "vk_memory_type_index": memory_type_index,
    })
}

/// The register-op payload a DMA-BUF-backed texture publishes.
///
/// Carries the DRM format modifier and per-plane row pitch so the
/// consumer-side EGL or Vulkan import can pass them via
/// `EGL_DMA_BUF_PLANE0_MODIFIER_LO/HI_EXT` and
/// `EGL_DMA_BUF_PLANE{N}_PITCH_EXT` (or
/// `VkImageDrmFormatModifierExplicitCreateInfoEXT`). The tiling rides along
/// because a zero modifier is `DRM_FORMAT_MOD_LINEAR` under
/// `DRM_FORMAT_MODIFIER_EXT` tiling and "no modifier at all" otherwise — the
/// value alone cannot say which. Render-target consumers must refuse a LINEAR
/// surface because LINEAR DMA-BUFs are sampler-only on NVIDIA (see
/// `docs/learnings/nvidia-egl-dmabuf-render-target.md`).
#[cfg(target_os = "linux")]
fn dma_buf_texture_registration_payload(
    texture: &crate::vulkan::rhi::HostVulkanTexture,
    surface_id: &str,
    runtime_id: &str,
    has_produce_done_fd: bool,
    has_consume_done_fd: bool,
    current_image_layout: streamlib_consumer_rhi::VulkanLayout,
) -> serde_json::Value {
    let plane_layout = texture
        .dma_buf_plane_layout()
        .unwrap_or_else(|_| vec![(0, 0)]);
    let plane_offsets: Vec<u64> = plane_layout.iter().map(|(o, _)| *o).collect();
    let plane_strides: Vec<u64> = plane_layout.iter().map(|(_, s)| *s).collect();

    serde_json::json!({
        "op": "register",
        "surface_id": surface_id,
        "runtime_id": runtime_id,
        "width": texture.width(),
        "height": texture.height(),
        "format": texture.format().wire_name(),
        "resource_type": SURFACE_RESOURCE_TYPE_TEXTURE,
        "plane_offsets": plane_offsets,
        "plane_strides": plane_strides,
        "drm_format_modifier": texture.chosen_drm_format_modifier(),
        "vk_image_tiling": texture.vk_image_tiling().as_raw(),
        // The host allocation's byte size, which a consumer-side
        // `import_render_target_dma_buf` must pass to `vkAllocateMemory` —
        // deriving it from extent × stride under-sizes tiled allocations and
        // fails the bind.
        "vk_image_allocation_size": texture.vma_allocation_size() as u64,
        SURFACE_REPLY_HAS_PRODUCE_DONE_FD: has_produce_done_fd,
        SURFACE_REPLY_HAS_CONSUME_DONE_FD: has_consume_done_fd,
        // The producer's declared `VkImageLayout`: the layout the texture
        // lives in immediately after registration, fed to host consumers as
        // the source layout of their first QFOT acquire barrier. Encoded as
        // i32 per the Vulkan spec.
        "current_image_layout": current_image_layout.as_vk().as_raw(),
    })
}

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

    /// **Engine-only** — register a pool slot with its cross-process timeline
    /// pair (macOS). See
    /// [`SurfaceStoreInner::register_pixel_buffer_with_timeline_pair`].
    #[cfg(target_os = "macos")]
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the macOS texture and escalate arms call it (#2402)"
        )
    )]
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

#[cfg(all(test, target_os = "linux"))]
mod plane_fd_ownership_tests {
    use super::*;
    use crate::core::rhi::RhiExternalHandle;

    /// A real fd standing in for one an exporter minted, paired with the
    /// identity that outlives its number being recycled — so "was this
    /// closed?" can be answered without racing a parallel test thread for
    /// the descriptor. A pipe rather than a device file because each pipe
    /// gets its own inode, and holding the write end keeps that inode
    /// alive even after the read end closes.
    struct ExportedPlaneFdUnderTest {
        plane_fd: std::os::unix::io::RawFd,
        write_end_fd: std::os::unix::io::RawFd,
        inode: u64,
    }

    fn inode_of(fd: std::os::unix::io::RawFd) -> Option<u64> {
        let mut file_status = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(fd, file_status.as_mut_ptr()) } != 0 {
            return None;
        }
        Some(unsafe { file_status.assume_init() }.st_ino as u64)
    }

    impl ExportedPlaneFdUnderTest {
        fn mint() -> Self {
            let mut pipe_ends = [0 as std::os::unix::io::RawFd; 2];
            assert_eq!(
                unsafe { libc::pipe2(pipe_ends.as_mut_ptr(), libc::O_CLOEXEC) },
                0,
                "could not mint a stand-in plane fd"
            );
            let inode = inode_of(pipe_ends[0]).expect("a fresh pipe must stat");
            Self {
                plane_fd: pipe_ends[0],
                write_end_fd: pipe_ends[1],
                inode,
            }
        }

        /// Closed outright, or its number recycled onto something else —
        /// either way the descriptor is no longer the one we handed over.
        fn was_closed(&self) -> bool {
            inode_of(self.plane_fd) != Some(self.inode)
        }
    }

    impl Drop for ExportedPlaneFdUnderTest {
        fn drop(&mut self) {
            unsafe {
                if !self.was_closed() {
                    libc::close(self.plane_fd);
                }
                libc::close(self.write_end_fd);
            }
        }
    }

    /// A single-flavour export holds every plane fd open for the send, and
    /// dropping the wire handles is the one close each fd gets.
    #[test]
    fn a_single_flavour_export_owns_every_plane_fd_until_the_send_is_done() {
        let first_plane = ExportedPlaneFdUnderTest::mint();
        let second_plane = ExportedPlaneFdUnderTest::mint();

        let wire_handles = adopt_exported_planes_into_wire_handles(vec![
            RhiExternalHandle::DmaBuf {
                fd: first_plane.plane_fd,
                size: 4096,
            },
            RhiExternalHandle::DmaBuf {
                fd: second_plane.plane_fd,
                size: 2048,
            },
        ])
        .expect("a single-flavour export must be accepted");

        assert_eq!(wire_handles.handle_type, SURFACE_HANDLE_TYPE_DMA_BUF);
        assert_eq!(wire_handles.plane_sizes, vec![4096, 2048]);
        assert_eq!(wire_handles.plane_offsets, vec![0, 0]);
        assert!(
            !first_plane.was_closed() && !second_plane.was_closed(),
            "the plane fds must still be open — the send has not happened yet"
        );

        drop(wire_handles);

        assert!(
            first_plane.was_closed() && second_plane.was_closed(),
            "dropping the wire handles must close every plane fd it adopted"
        );
    }

    /// The refusal path obeys the same rule as the send path. A plane fd
    /// left open here is one no owner ever closes; a plane fd closed twice
    /// corrupts whichever subsystem the kernel hands the number to next,
    /// which is the teardown crash in #1880.
    #[test]
    fn a_mixed_flavour_export_is_refused_and_closes_every_plane_it_adopted() {
        let first_plane = ExportedPlaneFdUnderTest::mint();
        let second_plane = ExportedPlaneFdUnderTest::mint();

        let refusal = match adopt_exported_planes_into_wire_handles(vec![
            RhiExternalHandle::DmaBuf {
                fd: first_plane.plane_fd,
                size: 4096,
            },
            RhiExternalHandle::OpaqueFd {
                fd: second_plane.plane_fd,
                size: 4096,
            },
        ]) {
            Ok(_) => panic!("a buffer exporting two flavours must be refused"),
            Err(refusal) => refusal,
        };

        assert!(
            refusal
                .to_string()
                .contains("mixed external-handle flavours"),
            "the refusal must name what it got: {refusal}"
        );
        assert!(
            first_plane.was_closed(),
            "the refusal left plane 0 open — no owner remains to close it"
        );
        assert!(
            second_plane.was_closed(),
            "the refusal left plane 1 open — no owner remains to close it"
        );
    }

    fn a_reply_announcing(produce_done: bool, consume_done: bool) -> serde_json::Value {
        serde_json::json!({
            SURFACE_REPLY_HAS_PRODUCE_DONE_FD: produce_done,
            SURFACE_REPLY_HAS_CONSUME_DONE_FD: consume_done,
        })
    }

    /// Stand-in fds handed over as the reply's owned fds, in order. The
    /// stand-ins outlive the owned fds so they can say which ones closed.
    fn reply_fds_standing_in_for(stand_ins: &[ExportedPlaneFdUnderTest]) -> Vec<OwnedFd> {
        stand_ins
            .iter()
            .map(|stand_in| unsafe { OwnedFd::from_raw_fd(stand_in.plane_fd) })
            .collect()
    }

    fn raw_fds_of(owned: &[OwnedFd]) -> Vec<std::os::unix::io::RawFd> {
        owned.iter().map(OwnedFd::as_raw_fd).collect()
    }

    #[test]
    fn a_reply_announcing_no_edges_keeps_every_fd_as_a_plane() {
        let stand_ins: Vec<_> = (0..3).map(|_| ExportedPlaneFdUnderTest::mint()).collect();
        let planes = plane_fds_of_reply(
            "lookup",
            &a_reply_announcing(false, false),
            reply_fds_standing_in_for(&stand_ins),
        )
        .expect("three fds and no edges are three planes");
        assert_eq!(
            raw_fds_of(&planes),
            stand_ins.iter().map(|p| p.plane_fd).collect::<Vec<_>>()
        );
        assert!(stand_ins.iter().all(|plane| !plane.was_closed()));
        drop(planes);
        assert!(stand_ins.iter().all(|plane| plane.was_closed()));
    }

    /// The service appends `produce_done` then `consume_done` after the
    /// planes; each announced edge comes off the end and closes, and the
    /// planes in front of them are untouched.
    #[test]
    fn each_announced_edge_is_peeled_off_the_end_and_closed() {
        for (produce_done, consume_done) in [(true, false), (false, true), (true, true)] {
            let edge_count = usize::from(produce_done) + usize::from(consume_done);
            let stand_ins: Vec<_> = (0..2 + edge_count)
                .map(|_| ExportedPlaneFdUnderTest::mint())
                .collect();
            let planes = plane_fds_of_reply(
                "lookup",
                &a_reply_announcing(produce_done, consume_done),
                reply_fds_standing_in_for(&stand_ins),
            )
            .expect("two planes remain once the edges are peeled");
            assert_eq!(
                raw_fds_of(&planes),
                stand_ins[..2]
                    .iter()
                    .map(|p| p.plane_fd)
                    .collect::<Vec<_>>(),
                "produce_done={produce_done} consume_done={consume_done}"
            );
            assert!(
                stand_ins[2..].iter().all(|edge| edge.was_closed()),
                "an announced edge must close with the peel (produce_done={produce_done} consume_done={consume_done})"
            );
            assert!(stand_ins[..2].iter().all(|plane| !plane.was_closed()));
            drop(planes);
        }
    }

    #[test]
    fn a_reply_shorter_than_its_flags_promise_is_refused_and_closes_what_it_carried() {
        let stand_ins: Vec<_> = (0..2).map(|_| ExportedPlaneFdUnderTest::mint()).collect();
        let refusal = plane_fds_of_reply(
            "lookup",
            &a_reply_announcing(true, true),
            reply_fds_standing_in_for(&stand_ins),
        )
        .expect_err("two fds cannot carry a plane and two edges");
        assert!(
            refusal
                .to_string()
                .contains("carried 2 fds, fewer than the 3"),
            "the refusal must name both counts: {refusal}"
        );
        assert!(
            stand_ins.iter().all(|fd| fd.was_closed()),
            "a refused reply must close every fd it carried"
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod dma_buf_registration_payload_tests {
    use super::*;
    use crate::core::rhi::{TextureDescriptor, TextureFormat, TextureUsages};
    use crate::vulkan::rhi::{HostVulkanDevice, HostVulkanTexture};

    /// The wire half of #1915: a surface whose driver-chosen modifier is
    /// `DRM_FORMAT_MOD_LINEAR` publishes a zero modifier, which reads exactly
    /// like the zero published by an image that never went through
    /// `VK_EXT_image_drm_format_modifier`. Only the tiling beside it tells a
    /// consumer which one it is holding, so the payload must carry the
    /// texture's recorded tiling rather than assume one.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_linear_modifier_surface_publishes_the_tiling_that_reads_its_modifier() {
        const DRM_FORMAT_MOD_LINEAR: u64 = 0;
        const VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT: i64 = 1_000_158_000;

        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(e) => {
                println!("Skipping — no Vulkan device: {e}");
                return;
            }
        };
        if device.dma_buf_image_pool_tiled().is_none() {
            println!("Skipping — tiled DMA-BUF pool not created");
            return;
        }
        let desc = TextureDescriptor::new(64, 64, TextureFormat::Bgra8Unorm).with_usage(
            TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST | TextureUsages::COPY_SRC,
        );
        let texture = match HostVulkanTexture::new_render_target_dma_buf(
            &device,
            &desc,
            &[DRM_FORMAT_MOD_LINEAR],
        ) {
            Ok(t) => t,
            Err(e) => {
                println!("Skipping — allocation against DRM_FORMAT_MOD_LINEAR refused: {e}");
                return;
            }
        };

        let payload = dma_buf_texture_registration_payload(
            &texture,
            "surface-under-test",
            "runtime-under-test",
            false,
            false,
            streamlib_consumer_rhi::VulkanLayout::UNDEFINED,
        );

        assert_eq!(
            payload.get("drm_format_modifier").and_then(|v| v.as_u64()),
            Some(DRM_FORMAT_MOD_LINEAR),
            "the driver chose LINEAR, whose modifier value is zero"
        );
        assert_eq!(
            payload.get("vk_image_tiling").and_then(|v| v.as_i64()),
            Some(VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT),
            "without this, the zero modifier above is indistinguishable on the \
             wire from an image that never carried a modifier at all"
        );
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod surface_export_staging_registration_payload_tests {
    use super::*;

    #[test]
    fn a_staging_registration_states_the_exporters_memory_type_index() {
        const EXPORTER_MEMORY_TYPE_INDEX: u32 = 4;

        let payload = surface_export_staging_registration_payload(
            "surface-under-test-hostvisible",
            "runtime-under-test",
            64 * 64 * 4,
            64,
            64,
            PixelFormat::Rgba32,
            EXPORTER_MEMORY_TYPE_INDEX,
        );

        assert_eq!(
            payload
                .get("vk_memory_type_index")
                .and_then(|value| value.as_u64()),
            Some(EXPORTER_MEMORY_TYPE_INDEX as u64),
            "without this the child picks a memory type by first match, which agrees \
             with a host-cached exporter only by coincidence"
        );
        assert_eq!(
            payload.get("handle_type").and_then(|value| value.as_str()),
            Some(SURFACE_HANDLE_TYPE_OPAQUE_FD),
            "the index is only meaningful because this is an OPAQUE_FD registration"
        );
    }

    /// Zero is a real memory type index, so it must survive onto the wire
    /// rather than reading as "absent" the way a defaulted field would.
    #[test]
    fn a_staging_registration_states_memory_type_index_zero_rather_than_omitting_it() {
        let payload = surface_export_staging_registration_payload(
            "surface-under-test-hostvisible",
            "runtime-under-test",
            4096,
            32,
            32,
            PixelFormat::Rgba32,
            0,
        );
        assert_eq!(
            payload
                .get("vk_memory_type_index")
                .and_then(|value| value.as_u64()),
            Some(0),
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod fd_ownership_tests {
    use super::*;
    use crate::linux::surface_share::{SurfaceShareState, UnixSocketSurfaceService};
    use std::os::unix::io::RawFd;
    use std::os::unix::net::UnixStream;

    /// The id the pool registers a slot under; a texture lookup of it is
    /// refused because the slot is a pixel buffer, never a texture.
    const PIXEL_BUFFER_SLOT_ID: &str = "fd-ownership-test-pool-slot";

    /// How many descriptors of this process name the memfd `plane_name` —
    /// the registration's own copy plus whatever a lookup left behind.
    /// Counting by name keeps the measure immune to the other tests
    /// opening and closing descriptors in the same process.
    fn open_descriptors_of_the_plane(plane_name: &str) -> usize {
        std::fs::read_dir("/proc/self/fd")
            .expect("/proc/self/fd is readable")
            .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
            .filter(|target| target.to_string_lossy().contains(plane_name))
            .count()
    }

    /// The service thread runs in this process too, and it closes its dup
    /// of the plane only after `sendmsg` has already delivered a copy to
    /// the lookup, so a count taken the instant a reply lands can include
    /// a copy that is about to close. A leak never settles back; a dup in
    /// flight does within microseconds.
    fn open_descriptors_of_the_plane_once_the_service_thread_settled(
        plane_name: &str,
        expected: usize,
    ) -> usize {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let count = open_descriptors_of_the_plane(plane_name);
            if count == expected || std::time::Instant::now() >= deadline {
                return count;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn memfd_named(plane_name: &str, bytes: &[u8]) -> RawFd {
        use std::io::Write;
        use std::os::unix::io::{FromRawFd, IntoRawFd};
        let name = std::ffi::CString::new(plane_name).unwrap();
        let fd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
        assert!(fd >= 0, "memfd_create: {}", std::io::Error::last_os_error());
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.write_all(bytes).expect("memfd write");
        file.into_raw_fd()
    }

    /// A live service with one pixel-buffer slot registered exactly the way
    /// the pixel-buffer pool registers its slots — `format` spelled in the
    /// pixel-buffer vocabulary, one plane — and a connected store.
    fn store_against_a_service_holding_one_pixel_buffer_slot(
        plane_name: &str,
    ) -> (
        tempfile::TempDir,
        UnixSocketSurfaceService,
        UnixStream,
        SurfaceStore,
    ) {
        let socket_dir = tempfile::TempDir::new().expect("temp dir for the test socket");
        let socket_path = socket_dir.path().join("surface-share.sock");
        let mut service =
            UnixSocketSurfaceService::new(SurfaceShareState::new(), socket_path.clone());
        service.start().expect("service start");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !socket_path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let registering_connection =
            UnixStream::connect(&socket_path).expect("connect to register");
        let plane = memfd_named(plane_name, &[0u8; 64]);
        let (response, no_reply_fds) = streamlib_surface_client::send_request_with_fds(
            &registering_connection,
            &serde_json::json!({
                "op": "register",
                "surface_id": PIXEL_BUFFER_SLOT_ID,
                "runtime_id": "fd-ownership-test-runtime",
                "width": 4,
                "height": 4,
                "format": crate::core::rhi::PixelFormat::Rgba32.wire_name(),
                "resource_type": SURFACE_RESOURCE_TYPE_PIXEL_BUFFER,
                "handle_type": "dma_buf",
                "plane_sizes": [64],
                "plane_offsets": [0],
                "plane_strides": [16],
            }),
            &[plane],
            0,
        )
        .expect("register request");
        unsafe { libc::close(plane) };
        assert!(no_reply_fds.is_empty());
        assert!(
            response.get("error").is_none(),
            "registration refused: {response}"
        );

        let store = SurfaceStore::new(
            socket_path.to_string_lossy().into_owned(),
            "fd-ownership-test-runtime".to_string(),
        );
        store.connect().expect("store connects to the service");
        (socket_dir, service, registering_connection, store)
    }

    #[test]
    fn a_texture_lookup_of_a_pixel_buffer_slot_is_refused_and_closes_the_plane_it_received() {
        const PLANE: &str = "fd-ownership-test-plane-for-texture-lookup";
        let (_socket_dir, mut service, _registering_connection, store) =
            store_against_a_service_holding_one_pixel_buffer_slot(PLANE);

        let descriptors_before_any_lookup = open_descriptors_of_the_plane(PLANE);
        for _ in 0..9 {
            assert!(
                store.lookup_texture(PIXEL_BUFFER_SLOT_ID).is_err(),
                "a pixel-buffer slot is not a texture"
            );
        }
        assert_eq!(
            open_descriptors_of_the_plane_once_the_service_thread_settled(
                PLANE,
                descriptors_before_any_lookup
            ),
            descriptors_before_any_lookup,
            "every refused texture lookup must close the plane fd the service sent with its reply"
        );
        service.stop();
    }

    /// Without an import device the lookup is refused before any Vulkan
    /// call, so on a machine with a GPU the test brings one up itself;
    /// otherwise which refusal it proves would depend on whether an earlier
    /// test in the binary happened to.
    #[test]
    fn a_buffer_lookup_that_cannot_import_closes_the_planes_it_received() {
        const PLANE: &str = "fd-ownership-test-plane-for-buffer-lookup";
        let import_device_is_up = crate::vulkan::rhi::vulkan_buffer::VULKAN_DEVICE_FOR_IMPORT
            .get()
            .is_some()
            || crate::core::rhi::GpuDevice::new().is_ok();
        if !import_device_is_up {
            tracing::warn!(
                "no Vulkan device — this run proves the no-device refusal, not the DMA-BUF check"
            );
        }
        let (_socket_dir, mut service, _registering_connection, store) =
            store_against_a_service_holding_one_pixel_buffer_slot(PLANE);

        let descriptors_before_any_lookup = open_descriptors_of_the_plane(PLANE);
        for _ in 0..9 {
            let Err(refusal) = store.lookup_buffer(PIXEL_BUFFER_SLOT_ID) else {
                panic!("a memfd is not a DMA-BUF, so the import must refuse");
            };
            if import_device_is_up {
                assert!(
                    refusal.to_string().contains("is not a DMA-BUF"),
                    "with an import device up, the refusal must come before the driver call: {refusal}"
                );
            }
        }
        assert_eq!(
            open_descriptors_of_the_plane_once_the_service_thread_settled(
                PLANE,
                descriptors_before_any_lookup
            ),
            descriptors_before_any_lookup,
            "every failed buffer lookup must close the plane fds the service sent with its reply"
        );
        service.stop();
    }
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod mach_surface_share_pool_tests {
    use std::time::Duration;

    use objc2_io_surface::IOSurfaceRef;
    use streamlib_surface_client::SurfaceShareMachServiceConnection;

    use crate::apple::surface_share::{IOSurfaceShareState, MachSurfaceShareService};
    use crate::core::context::{GpuContext, SurfaceStore};
    use crate::core::rhi::{PixelFormat, pool_slot_key_of_surface_id};

    fn gpu_or_skip() -> Option<GpuContext> {
        match GpuContext::init_for_platform() {
            Ok(gpu) => Some(gpu),
            Err(e) => {
                tracing::warn!("skipping — no GPU device: {e}");
                None
            }
        }
    }

    fn engine_pattern_byte(index: usize) -> u8 {
        (index.wrapping_mul(13).wrapping_add(5)) as u8
    }

    /// A pooled frame is an IOSurface the service hands out: another
    /// connection checks the published frame id out, and the surface its port
    /// names holds the pixels the producer wrote into the pooled buffer.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_pooled_frame_crosses_the_mach_service_as_the_iosurface_holding_its_pixels() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let state = IOSurfaceShareState::new();
        let mut service = MachSurfaceShareService::new(
            state.clone(),
            format!(
                "com.tatolab.streamlib.surface-share-test.pool.{}",
                std::process::id()
            ),
        );
        service.start().expect("the service starts");
        let store = SurfaceStore::new_reading_check_out_leases(
            service.service_name().to_string(),
            "R-pool-test".to_string(),
            std::sync::Arc::clone(state.check_out_leases()),
        );
        store.connect().expect("the store connects");
        gpu.set_surface_store(store);

        let (frame_id, pooled_buffer) = gpu
            .acquire_pixel_buffer(64, 32, PixelFormat::Bgra32)
            .expect("a pooled frame");
        let pooled_bytes = unsafe {
            std::slice::from_raw_parts_mut(
                pooled_buffer.buffer_ref().inner.mapped_ptr(),
                64 * 32 * 4,
            )
        };
        for (index, byte) in pooled_bytes.iter_mut().enumerate() {
            *byte = engine_pattern_byte(index);
        }
        assert!(
            state
                .surface_ids()
                .contains(&pool_slot_key_of_surface_id(frame_id.to_string().as_str()).to_string()),
            "the pool registered its slot with the service"
        );

        let reader = SurfaceShareMachServiceConnection::connect(
            service.service_name(),
            Duration::from_secs(10),
        )
        .expect("a reader connects");
        let (answer, ports) = reader
            .send_request_with_ports(
                &serde_json::json!({"op": "check_out", "surface_id": frame_id.to_string()}),
                Vec::new(),
            )
            .expect("check_out round-trip");
        assert!(answer.get("error").is_none(), "{answer}");
        assert_eq!(answer["plane_strides"], serde_json::json!([64 * 4]));
        let iosurface = IOSurfaceRef::lookup_from_mach_port(ports[0].as_raw_name())
            .expect("the port names the slot's IOSurface");
        let shared_bytes = unsafe {
            std::slice::from_raw_parts(iosurface.base_address().as_ptr().cast::<u8>(), 64 * 32 * 4)
        };
        assert!(
            shared_bytes
                .iter()
                .enumerate()
                .all(|(index, byte)| *byte == engine_pattern_byte(index)),
            "the shared surface holds the pooled buffer's pixels"
        );
    }

    /// A started service with this process's store connected to it, the way
    /// the runtime's `start()` builds one.
    fn a_store_connected_to_a_started_service(
        gpu: &GpuContext,
        label: &str,
    ) -> (IOSurfaceShareState, MachSurfaceShareService) {
        let state = IOSurfaceShareState::new();
        let mut service = MachSurfaceShareService::new(
            state.clone(),
            format!(
                "com.tatolab.streamlib.surface-share-test.{label}.{}",
                std::process::id()
            ),
        );
        service.start().expect("the service starts");
        let store = SurfaceStore::new_sharing_the_mach_services_tables(
            service.service_name().to_string(),
            "R-store-test".to_string(),
            std::sync::Arc::clone(state.check_out_leases()),
            std::sync::Arc::clone(state.cross_process_timeline_pairs()),
        );
        store.connect().expect("the store connects");
        gpu.set_surface_store(store);
        (state, service)
    }

    fn a_timeline_pair(
        gpu: &GpuContext,
        exportable: bool,
    ) -> std::sync::Arc<crate::apple::surface_share::CrossProcessTimelinePair> {
        use crate::vulkan::rhi::HostVulkanTimelineSemaphore;
        let device = gpu.device().inner.device();
        let timeline = || {
            std::sync::Arc::new(if exportable {
                HostVulkanTimelineSemaphore::new_exportable(device, 0).expect("a timeline")
            } else {
                HostVulkanTimelineSemaphore::new(device, 0).expect("a timeline")
            })
        };
        std::sync::Arc::new(crate::apple::surface_share::CrossProcessTimelinePair::new(
            timeline(),
            timeline(),
        ))
    }

    /// A pool slot registered with its timeline pair checks out with both
    /// shared-event ports after its IOSurface's, and the service can reach
    /// the engine's pair for a helper's host-side reports.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_slot_registered_with_its_timeline_pair_checks_out_with_both_shared_event_ports() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let (state, service) = a_store_connected_to_a_started_service(&gpu, "timeline-pair");
        let store = gpu.surface_store().expect("the store");
        let (_, pixel_buffer) = gpu
            .acquire_pixel_buffer(16, 8, PixelFormat::Bgra32)
            .expect("a pooled frame");
        let pair = a_timeline_pair(&gpu, true);

        store
            .host_register_pixel_buffer_with_timeline_pair("slot-with-pair", &pixel_buffer, &pair)
            .expect("the registration crosses");

        let reader = SurfaceShareMachServiceConnection::connect(
            service.service_name(),
            Duration::from_secs(10),
        )
        .expect("a reader connects");
        let (answer, ports) = reader
            .send_request_with_ports(
                &serde_json::json!({"op": "check_out", "surface_id": "slot-with-pair"}),
                Vec::new(),
            )
            .expect("check_out round-trip");
        assert_eq!(answer["has_produce_done_port"], true, "{answer}");
        assert_eq!(ports.len(), 3);
        assert!(!pair.orders_host_side());
        assert!(
            state
                .cross_process_timeline_pairs()
                .pair_of("slot-with-pair")
                .is_some()
        );
    }

    /// A second registration under a live id is refused, and the first
    /// registration keeps its pair.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_refused_duplicate_registration_leaves_the_live_registrations_pair() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let (state, _service) = a_store_connected_to_a_started_service(&gpu, "timeline-dup");
        let store = gpu.surface_store().expect("the store");
        let (_, pixel_buffer) = gpu
            .acquire_pixel_buffer(16, 8, PixelFormat::Bgra32)
            .expect("a pooled frame");
        let live_pair = a_timeline_pair(&gpu, true);
        store
            .host_register_pixel_buffer_with_timeline_pair("slot-dup", &pixel_buffer, &live_pair)
            .expect("the first registration crosses");

        store
            .host_register_pixel_buffer_with_timeline_pair(
                "slot-dup",
                &pixel_buffer,
                &a_timeline_pair(&gpu, true),
            )
            .expect_err("a second registration under a live id is refused");

        let kept = state
            .cross_process_timeline_pairs()
            .pair_of("slot-dup")
            .expect("the live registration keeps a pair");
        assert!(std::sync::Arc::ptr_eq(&kept, &live_pair));
    }

    /// A pair whose timelines will not export crosses without ports and
    /// orders host-side, without a rebuild or a refused registration.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_slot_whose_timelines_will_not_export_registers_ordering_host_side() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let (_state, service) = a_store_connected_to_a_started_service(&gpu, "timeline-fallback");
        let store = gpu.surface_store().expect("the store");
        let (_, pixel_buffer) = gpu
            .acquire_pixel_buffer(16, 8, PixelFormat::Bgra32)
            .expect("a pooled frame");
        let pair = a_timeline_pair(&gpu, false);

        store
            .host_register_pixel_buffer_with_timeline_pair("slot-host-side", &pixel_buffer, &pair)
            .expect("the registration still crosses");

        let reader = SurfaceShareMachServiceConnection::connect(
            service.service_name(),
            Duration::from_secs(10),
        )
        .expect("a reader connects");
        let (answer, ports) = reader
            .send_request_with_ports(
                &serde_json::json!({"op": "lookup", "surface_id": "slot-host-side"}),
                Vec::new(),
            )
            .expect("lookup round-trip");
        assert_eq!(answer["has_produce_done_port"], false, "{answer}");
        assert_eq!(ports.len(), 1);
        assert!(pair.orders_host_side());
    }

    /// Register `iosurface` under `surface_id` from a connection of its own,
    /// stating `width` pixels a row.
    fn register_from_another_connection(
        service: &MachSurfaceShareService,
        surface_id: &str,
        iosurface: &IOSurfaceRef,
        width: u32,
    ) -> SurfaceShareMachServiceConnection {
        let registering_connection = SurfaceShareMachServiceConnection::connect(
            service.service_name(),
            Duration::from_secs(10),
        )
        .expect("a registering connection");
        let (registered, _) = registering_connection
            .send_request_with_ports(
                &serde_json::json!({
                    "op": "register",
                    "surface_id": surface_id,
                    "runtime_id": "R-elsewhere",
                    "width": width,
                    "height": iosurface.height(),
                    "format": "bgra32",
                }),
                vec![
                    crate::apple::iosurface::create_iosurface_mach_send_right(iosurface)
                        .expect("a port to the surface"),
                ],
            )
            .expect("register round-trip");
        assert_eq!(registered, serde_json::json!({"success": true}));
        registering_connection
    }

    /// The store resolves a surface another connection registered into a
    /// pixel buffer over its IOSurface, at the extent and format the service
    /// states, holding the registering side's pixels byte for byte.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn the_store_resolves_a_surface_registered_elsewhere_into_its_pixels() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let (_state, service) = a_store_connected_to_a_started_service(&gpu, "store-lookup");
        let iosurface = crate::apple::iosurface::create_private_iosurface_with_packed_rows(
            48,
            16,
            4,
            PixelFormat::Bgra32,
        )
        .expect("a private IOSurface");
        let surface_bytes = unsafe {
            std::slice::from_raw_parts_mut(
                iosurface.base_address().as_ptr().cast::<u8>(),
                48 * 16 * 4,
            )
        };
        for (index, byte) in surface_bytes.iter_mut().enumerate() {
            *byte = engine_pattern_byte(index);
        }
        let _registering_connection =
            register_from_another_connection(&service, "slot-elsewhere", &iosurface, 48);

        let store = gpu.surface_store().expect("the store");
        for resolved in [
            store
                .lookup_buffer("slot-elsewhere")
                .expect("lookup_buffer resolves it"),
            store
                .check_out("slot-elsewhere")
                .expect("check_out resolves it"),
        ] {
            assert_eq!((resolved.width, resolved.height), (48, 16));
            assert_eq!(resolved.format(), PixelFormat::Bgra32);
            let resolved_bytes = unsafe {
                std::slice::from_raw_parts(resolved.buffer_ref().inner.mapped_ptr(), 48 * 16 * 4)
            };
            assert!(
                resolved_bytes
                    .iter()
                    .enumerate()
                    .all(|(index, byte)| *byte == engine_pattern_byte(index)),
                "the resolved buffer holds the registered surface's pixels"
            );
        }
    }

    /// A surface whose rows do not pack at the stated width is refused by
    /// name rather than read at the wrong stride.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_surface_whose_rows_do_not_pack_at_the_stated_width_is_refused() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        let (_state, service) = a_store_connected_to_a_started_service(&gpu, "store-stride");
        let seventeen_pixel_rows =
            crate::apple::iosurface::create_private_iosurface_with_packed_rows(
                17,
                8,
                4,
                PixelFormat::Bgra32,
            )
            .expect("a private IOSurface");
        let _registering_connection =
            register_from_another_connection(&service, "slot-padded", &seventeen_pixel_rows, 16);

        let refusal = gpu
            .surface_store()
            .expect("the store")
            .lookup_buffer("slot-padded")
            .expect_err("rows of 68 bytes are not 16 packed BGRA pixels");
        assert!(
            refusal.to_string().contains("rows are 68 bytes"),
            "the refusal names the stride: {refusal}"
        );
    }

    /// The pool never rehands a slot whose IOSurface the kernel reports in
    /// use — here by a use count this process holds, which counts the same
    /// as one a helper holds — and takes it back once the use ends.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_slot_whose_iosurface_is_in_use_is_not_rehanded_to_its_producer() {
        let Some(gpu) = gpu_or_skip() else {
            return;
        };
        // Every acquisition below is dropped at once, so the ring never grows
        // past its pre-allocated slots and this many visits each several times.
        let acquisitions_that_revisit_every_slot = 16;
        let slot_key_of = |frame_id: &crate::core::rhi::PublishedPixelBufferFrameId| {
            pool_slot_key_of_surface_id(frame_id.to_string().as_str()).to_string()
        };

        let (held_frame_id, held_buffer) = gpu
            .acquire_pixel_buffer(32, 32, PixelFormat::Bgra32)
            .expect("a pooled frame");
        let held_slot_key = slot_key_of(&held_frame_id);
        let held_iosurface = held_buffer
            .buffer_ref()
            .inner
            .backing_iosurface()
            .map(objc2_core_foundation::CFRetained::<IOSurfaceRef>::from)
            .expect("a macOS pool slot is an IOSurface");
        held_iosurface.increment_use_count();
        drop(held_buffer);

        for _ in 0..acquisitions_that_revisit_every_slot {
            let (frame_id, _) = gpu
                .acquire_pixel_buffer(32, 32, PixelFormat::Bgra32)
                .expect("a pooled frame");
            assert_ne!(
                slot_key_of(&frame_id),
                held_slot_key,
                "a slot in use was rehanded"
            );
        }

        held_iosurface.decrement_use_count();
        let the_slot_came_back = (0..acquisitions_that_revisit_every_slot).any(|_| {
            let (frame_id, _) = gpu
                .acquire_pixel_buffer(32, 32, PixelFormat::Bgra32)
                .expect("a pooled frame");
            slot_key_of(&frame_id) == held_slot_key
        });
        assert!(the_slot_came_back, "the slot returns once nothing uses it");
    }
}
