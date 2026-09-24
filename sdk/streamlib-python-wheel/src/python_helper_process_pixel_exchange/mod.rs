// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The helper child's half of the pixel exchange.
//!
//! A Python processor's pixels live in the engine's pools, one process away.
//! Acquiring goes through two channels that already exist: the escalate
//! socket carries the request (the parent allocates, checks the buffer into
//! its surface-share service, and answers with the minted surface id), and
//! the surface-share socket carries the memory itself — the checkout's
//! `recvmsg` lands the DMA-BUF plane fds in this process's fd table via
//! SCM_RIGHTS, and the consumer-side Vulkan import maps them. The escalate
//! socket never carries fds.
//!
//! Pool allocations are DMA-BUF-flavoured, and external device APIs
//! import OPAQUE_FD — one allocation cannot export both on NVIDIA. So
//! the device side goes through the parent's per-surface staging buffer:
//! `open_device_export_staging` has the parent publish that staging and
//! its refill timeline, the checkout delivers both fds, CUDA imports the
//! memory, and each refill is an escalate round trip whose answer is the
//! timeline value to wait for. The host's own wait after a refill orders
//! nothing for this process; the timeline does.

#[cfg(target_os = "linux")]
use std::path::PathBuf;

use pyo3::prelude::*;
use pyo3::types::PyDict;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use pyo3::exceptions::PyRuntimeError;
#[cfg(target_os = "linux")]
use std::os::fd::OwnedFd;
#[cfg(target_os = "linux")]
use std::os::unix::net::UnixStream;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::sync::Arc;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use parking_lot::Mutex;
#[cfg(target_os = "linux")]
use streamlib_consumer_rhi::ConsumerVulkanBuffer;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use streamlib_consumer_rhi::ConsumerVulkanDevice;

use streamlib::sdk::rhi::PixelFormat;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod gpu_kernels;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use gpu_kernels::{
    HelperProcessGraphicsDraw, HelperProcessGraphicsKernelRegistration,
    HelperProcessRayTracingKernelRegistration, compute_dispatch_wire_entry,
};

#[cfg(target_os = "linux")]
pub(crate) use linux::{
    CpuReadbackCopyDirection, HelperAcquiredTexture, HelperCheckedOutTextureSurface,
    HelperCpuReadbackExport, HelperDeviceExport, HelperForeignSurfaceUnregisterDebt,
    OpaqueFdTextureExportDescription,
};
#[cfg(target_os = "macos")]
pub(crate) use macos::{HelperCheckedOutTextureSurface, IOSurfaceMachPortExportDescription};
#[cfg(target_os = "macos")]
use macos::{
    HelperIOSurfaceCpuLock, HelperIOSurfaceImportsByPoolSlot, HelperIOSurfacePoolSlotImport,
    HelperIOSurfaceUseCountClaim,
};

/// One field of an escalate response, named in the failure so a parent
/// that answered a shape this child does not understand says which part.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn response_field<'py>(response: &Bound<'py, PyAny>, field: &str) -> PyResult<Bound<'py, PyAny>> {
    response.get_item(field).map_err(|_| {
        crate::python_processor_context::gpu_operation_error(format!(
            "the parent's response carried no {field}"
        ))
    })
}

/// One escalate round trip to the parent, called with the GIL attached.
///
/// The callable is the bridge's `request_from_parent`, whose wait on the
/// response releases the GIL — a slow parent parks this thread, never the
/// interpreter's others.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn escalate_round_trip_to_parent<'py>(
    python: Python<'py>,
    escalate_request_to_parent: &Py<PyAny>,
    op: &Bound<'py, PyDict>,
) -> PyResult<Bound<'py, PyAny>> {
    escalate_request_to_parent
        .bind(python)
        .call1((op,))
        .map_err(|request_failure| {
            crate::python_processor_context::gpu_operation_error(format!(
                "the parent refused or failed the GPU request: {request_failure}"
            ))
        })
}

/// Hand the parent's `release_handle` for `handle_id` to the bridge's release
/// worker and return without waiting on its answer.
///
/// The callable is the bridge's `release_to_parent_without_waiting`. Every
/// release a drop owes goes this way: the drop can be a garbage-collector
/// finalizer on the bridge's reader, which a round trip would stall for the
/// whole escalate timeout, since only that thread delivers the answer.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn hand_a_release_handle_to_the_release_worker(
    python: Python<'_>,
    release_to_parent_without_waiting: &Py<PyAny>,
    handle_id: &str,
) -> PyResult<()> {
    let release_op = PyDict::new(python);
    release_op.set_item("op", "release_handle")?;
    release_op.set_item("handle_id", handle_id)?;
    release_to_parent_without_waiting
        .bind(python)
        .call1((release_op,))?;
    Ok(())
}

/// One `u32` field of a checkout's registration metadata, present and
/// positive or refused naming the field.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn required_positive_u32_check_out_metadata_field(
    response: &serde_json::Value,
    surface_id: &str,
    field: &str,
) -> PyResult<u32> {
    response
        .get(field)
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            PyRuntimeError::new_err(format!(
                "check_out of {surface_id:?} carried no usable {field}"
            ))
        })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
use vk_image_creation_recipe_wire_parse::vk_image_creation_recipe_of_check_out;

/// The `VkImageCreateInfo` recipe a texture checkout carries, parsed the
/// same way on both floors.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod vk_image_creation_recipe_wire_parse {
    /// Absent-defaults for the `vk_image_*` recipe fields, mirroring the
    /// surface-share wire's documented defaults
    /// (`core/context/surface_share_wire_verbs.rs`).
    pub(super) const VK_IMAGE_TILING_DEFAULT: i32 = 0; // VK_IMAGE_TILING_OPTIMAL
    pub(super) const VK_IMAGE_MIP_LEVELS_DEFAULT: u32 = 1;
    pub(super) const VK_IMAGE_ARRAY_LAYERS_DEFAULT: u32 = 1;
    pub(super) const VK_IMAGE_SAMPLES_DEFAULT: i32 = 1; // VK_SAMPLE_COUNT_1_BIT
    /// `TRANSFER_SRC (0x01) | TRANSFER_DST (0x02) | SAMPLED (0x04) | STORAGE (0x08)`.
    pub(super) const VK_IMAGE_USAGE_DEFAULT: u32 = 0x0F;

    /// One `i32` recipe field of a checkout's registration metadata,
    /// absent-or-unrepresentable defaulting to the service's documented value.
    fn defaulted_i32_check_out_metadata_field(
        response: &serde_json::Value,
        field: &str,
        default: i32,
    ) -> i32 {
        response
            .get(field)
            .and_then(|value| value.as_i64())
            .and_then(|value| i32::try_from(value).ok())
            .unwrap_or(default)
    }

    /// The `u32` twin of [`defaulted_i32_check_out_metadata_field`].
    fn defaulted_u32_check_out_metadata_field(
        response: &serde_json::Value,
        field: &str,
        default: u32,
    ) -> u32 {
        response
            .get(field)
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(default)
    }

    /// The `VkImageCreateInfo` recipe a texture checkout's registration carries,
    /// each field absent-defaulting to the service's documented value.
    pub(super) fn vk_image_creation_recipe_of_check_out(
        response: &serde_json::Value,
    ) -> crate::python_processor_context::ExportedVkImageCreationRecipe {
        crate::python_processor_context::ExportedVkImageCreationRecipe {
            vk_image_tiling: defaulted_i32_check_out_metadata_field(
                response,
                "vk_image_tiling",
                VK_IMAGE_TILING_DEFAULT,
            ),
            vk_image_usage_flags: defaulted_u32_check_out_metadata_field(
                response,
                "vk_image_usage",
                VK_IMAGE_USAGE_DEFAULT,
            ),
            vk_image_mip_levels: defaulted_u32_check_out_metadata_field(
                response,
                "vk_image_mip_levels",
                VK_IMAGE_MIP_LEVELS_DEFAULT,
            ),
            vk_image_array_layers: defaulted_u32_check_out_metadata_field(
                response,
                "vk_image_array_layers",
                VK_IMAGE_ARRAY_LAYERS_DEFAULT,
            ),
            vk_image_samples: defaulted_i32_check_out_metadata_field(
                response,
                "vk_image_samples",
                VK_IMAGE_SAMPLES_DEFAULT,
            ),
        }
    }
}

/// Raise the service's own refusal of a checkout, if it refused one.
///
/// `checked_out_subject` names what was being checked out, because the caller
/// knows whether it asked for a published surface or the staging behind one and
/// the response does not. Taken as `format_args!` so the happy path — every
/// frame a consumer claims or resolves — formats nothing.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn refuse_check_out_the_service_declined(
    checked_out_subject: std::fmt::Arguments<'_>,
    response: &serde_json::Value,
) -> PyResult<()> {
    match response.get("error").and_then(|value| value.as_str()) {
        Some(checkout_error) => Err(PyRuntimeError::new_err(format!(
            "the surface-share service refused check_out of {checked_out_subject}: \
             {checkout_error}"
        ))),
        None => Ok(()),
    }
}

/// What a checkout turned into once its memory was imported: mapped memory
/// plus the layout facts every view derives from.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) struct HelperCheckedOutPixelSurface {
    /// The id this surface travels under — what a downstream processor
    /// resolves, and what keys the parent's registry entry.
    pub(crate) surface_id: String,
    #[cfg(target_os = "linux")]
    pub(crate) consumer_buffer: ConsumerVulkanBuffer,
    /// The pool slot's IOSurface and its import, shared with this helper's
    /// per-slot cache and every other frame checked out over the slot.
    #[cfg(target_os = "macos")]
    pub(crate) iosurface_pool_slot_import: Arc<HelperIOSurfacePoolSlotImport>,
    /// This frame's claim on the IOSurface's use count — the claim the kernel
    /// keeps truthful across processes, and drops if this process dies.
    #[cfg(target_os = "macos")]
    #[expect(
        dead_code,
        reason = "settled by its own Drop; nothing reads it, and that is the point"
    )]
    pub(crate) iosurface_use_count_claim: HelperIOSurfaceUseCountClaim,
    /// The IOSurface lock this surface's CPU access holds, if any.
    #[cfg(target_os = "macos")]
    pub(crate) iosurface_cpu_lock: HelperIOSurfaceCpuLock,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format: PixelFormat,
    pub(crate) bytes_per_row: u64,
    /// Present only on an acquired surface — a resolved one belongs to its
    /// acquirer, and releasing it here would evict somebody else's frame.
    pub(crate) release_to_parent: Option<HelperSurfaceReleaseDebt>,
    /// Present only on an adopted foreign DMA-BUF — the registration this
    /// import created is this surface's to remove; settled by its own Drop.
    #[cfg(target_os = "linux")]
    pub(crate) unregister_foreign_from_surface_share: Option<HelperForeignSurfaceUnregisterDebt>,
    /// The checkout lease this surface owes, whoever owns the surface itself.
    #[expect(
        dead_code,
        reason = "settled by its own Drop; nothing reads it, and that is the point"
    )]
    pub(crate) release_check_out_to_surface_share: HelperSurfaceCheckOutLeaseDebt,
    /// The plane fds this checkout was delivered, kept so
    /// `export_dma_buf` can answer from them. They are the same fds a
    /// host-side export would mint — the check-out is a kernel dup of
    /// that export — so the child answers locally instead of asking for
    /// something it already holds.
    #[cfg(target_os = "linux")]
    exported_plane_fds: Vec<OwnedFd>,
    /// The client this surface was checked out through, and the one its
    /// device export goes back to.
    #[cfg(target_os = "linux")]
    pub(crate) exchange_client: Arc<HelperProcessGpuExchangeClient>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl HelperCheckedOutPixelSurface {
    /// The host view of this surface's own mapping; null where the import
    /// has no host mapping.
    pub(crate) fn host_visible_pixel_plane_view(
        &self,
    ) -> crate::python_gpu_surface_pixel_exchange::HostVisiblePixelPlaneView {
        crate::python_gpu_surface_pixel_exchange::HostVisiblePixelPlaneView {
            base_address: self.host_mapped_base_address(),
            bytes_per_row: self.bytes_per_row,
            width: self.width,
            height: self.height,
            format: self.format,
        }
    }

    /// Where the CPU addresses this surface's pixels in this process, or
    /// null when its import has no host mapping.
    pub(crate) fn host_mapped_base_address(&self) -> *mut u8 {
        #[cfg(target_os = "linux")]
        return self.consumer_buffer.mapped_ptr();
        #[cfg(target_os = "macos")]
        return self.iosurface_pool_slot_import.consumer_buffer.mapped_ptr();
    }
}

/// The backings one surface id can stand for, behind one lifetime story:
/// the two a checkout imports, and the acquired device texture that was
/// never checked out at all — a name whose memory stays engine-side.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) enum HelperCheckedOutSurface {
    PixelBuffer(HelperCheckedOutPixelSurface),
    Texture(HelperCheckedOutTextureSurface),
    #[cfg(target_os = "linux")]
    AcquiredDeviceTexture(HelperAcquiredTexture),
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl HelperCheckedOutSurface {
    pub(crate) fn surface_id(&self) -> &str {
        match self {
            Self::PixelBuffer(pixel_surface) => &pixel_surface.surface_id,
            Self::Texture(texture_surface) => &texture_surface.surface_id,
            #[cfg(target_os = "linux")]
            Self::AcquiredDeviceTexture(acquired_texture) => &acquired_texture.surface_id,
        }
    }

    pub(crate) fn width(&self) -> u32 {
        match self {
            Self::PixelBuffer(pixel_surface) => pixel_surface.width,
            Self::Texture(texture_surface) => texture_surface.width,
            #[cfg(target_os = "linux")]
            Self::AcquiredDeviceTexture(acquired_texture) => acquired_texture.width,
        }
    }

    pub(crate) fn height(&self) -> u32 {
        match self {
            Self::PixelBuffer(pixel_surface) => pixel_surface.height,
            Self::Texture(texture_surface) => texture_surface.height,
            #[cfg(target_os = "linux")]
            Self::AcquiredDeviceTexture(acquired_texture) => acquired_texture.height,
        }
    }

    /// The snake-case format name the Python surface spells.
    pub(crate) fn format_wire_name(&self) -> &'static str {
        match self {
            Self::PixelBuffer(pixel_surface) => pixel_surface.format.wire_name(),
            Self::Texture(texture_surface) => texture_surface.format.wire_name(),
            #[cfg(target_os = "linux")]
            Self::AcquiredDeviceTexture(acquired_texture) => acquired_texture.format.wire_name(),
        }
    }
}

/// The release an acquired surface owes its parent: one `release_handle`
/// escalate op, which drops the parent registry's strong reference and the
/// surface-share service entry together.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) struct HelperSurfaceReleaseDebt {
    release_to_parent_without_waiting: Py<PyAny>,
    handle_id: String,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Drop for HelperSurfaceReleaseDebt {
    /// Best-effort: a parent that is already gone has released everything
    /// with the connection, so a failure here is logged, never raised.
    fn drop(&mut self) {
        Python::attach(|python| {
            if let Err(release_failure) = hand_a_release_handle_to_the_release_worker(
                python,
                &self.release_to_parent_without_waiting,
                &self.handle_id,
            ) {
                tracing::warn!(
                    "releasing surface {} to the parent failed ({release_failure}); its pool \
                     slot returns at teardown",
                    self.handle_id
                );
            }
        });
    }
}

/// The checkout lease a surface owes the surface-share service: one
/// `release_check_out`, over this helper's current connection. The service
/// frees only a lease the asking connection took, so a lease taken on a
/// connection set aside after a timeout is freed by nothing until this helper
/// stops.
///
/// Unlike [`HelperSurfaceReleaseDebt`] this unregisters nothing — it says only
/// "I am done reading". Owned by the surface, so it settles when the surface's
/// `GpuSurfaceOwnedMemory` loses its last share, handle *and* every exported
/// view: paying it at `close()` would return the slot under a live tensor.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) struct HelperSurfaceCheckOutLeaseDebt {
    exchange_client: Arc<HelperProcessGpuExchangeClient>,
    surface_id: String,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Drop for HelperSurfaceCheckOutLeaseDebt {
    /// Best-effort: a parent that is already gone dropped this connection, and
    /// the service reclaims every lease on a connection's socket closing — so
    /// a failure here is logged, never raised.
    fn drop(&mut self) {
        Python::attach(|python| {
            // The round trip blocks, so it runs detached — this can be a
            // capsule deleter running under the child's GIL.
            let released =
                python.detach(|| self.exchange_client.release_check_out(&self.surface_id));
            if let Err(release_failure) = released {
                tracing::warn!(
                    "releasing the checkout of surface {} failed ({release_failure}); its pool \
                     slot returns when the connection it was claimed on closes, at the latest \
                     when this helper stops",
                    self.surface_id
                );
            }
        });
    }
}

/// What memory crosses the surface-share channel as: a descriptor over
/// SCM_RIGHTS on Linux.
#[cfg(target_os = "linux")]
type SurfaceShareTransferredHandle = OwnedFd;
/// What memory crosses the surface-share channel as: a Mach port right on
/// macOS.
#[cfg(target_os = "macos")]
type SurfaceShareTransferredHandle = streamlib_surface_client::OwnedMachSendRight;

/// A surface-share answer and the handles it carried.
#[cfg(any(target_os = "linux", target_os = "macos"))]
type SurfaceShareAnswer = (serde_json::Value, Vec<SurfaceShareTransferredHandle>);

/// How long the surface-share channel may keep a helper waiting: every
/// request on Linux, the connect handshake on macOS. A Mach request carries
/// no receive timeout; the service's death is what wakes one still pending.
///
/// The service answers from in-memory state and duplicated handles, never GPU
/// work, so this bounds only a service that has stopped answering — and with
/// it the time every other thread waits on the connection's lock.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const SURFACE_SHARE_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The child-side client that fulfills `ctx.gpu_limited_access` calls by
/// crossing to the parent: escalate for allocation, surface-share for the
/// memory, one consumer Vulkan device per child for the import.
pub(crate) struct HelperProcessGpuExchangeClient {
    #[cfg_attr(not(any(target_os = "linux", target_os = "macos")), expect(dead_code))]
    escalate_request_to_parent: Py<PyAny>,
    /// The bridge's door for a release that must not wait on its answer where
    /// it is owed — every release a drop owes.
    #[cfg_attr(not(any(target_os = "linux", target_os = "macos")), expect(dead_code))]
    release_to_parent_without_waiting: Py<PyAny>,
    #[cfg(target_os = "linux")]
    surface_socket_path: PathBuf,
    /// The bootstrap name of the parent's surface-share Mach service, as the
    /// environment carried it.
    #[cfg(target_os = "macos")]
    surface_share_mach_service_name: std::ffi::OsString,
    /// One connection per child, opened at first checkout; the service
    /// releases every claim it holds when it closes.
    #[cfg(target_os = "macos")]
    surface_share_mach_connection:
        Mutex<Option<Arc<streamlib_surface_client::SurfaceShareMachServiceConnection>>>,
    /// Each pool slot's IOSurface and its import, looked up once per slot
    /// rather than per frame: a child's first lookup of a surface costs
    /// 9–10 ms. Holding them does not pin a slot — only a frame's use-count
    /// claim does. Slot ids are never reused, so an entry never names the
    /// wrong surface; one whose pool the engine dropped stays retained until
    /// this helper stops or the parent goes away, which empties it.
    #[cfg(target_os = "macos")]
    iosurface_imports_by_pool_slot: HelperIOSurfaceImportsByPoolSlot,
    /// The runtime id this client's foreign-surface adoptions register and
    /// release under. Deliberately **not** the node's own runtime id: the
    /// service's crash watchdog releases every surface a disconnected
    /// subprocess registered *by runtime id*, so a child registering under
    /// the node's id would have its crash sweep the parent's own
    /// registrations. A child-scoped id makes the sweep exactly this
    /// child's adoptions.
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    foreign_surface_registration_runtime_id: String,
    /// One connection per child, opened at first checkout. Taken out for
    /// each exchange and put back only on success, so a stream with half a
    /// frame in it is dropped rather than reused.
    #[cfg(target_os = "linux")]
    surface_share_connection: Mutex<Option<UnixStream>>,
    /// Connections whose answer outwaited [`SURFACE_SHARE_RESPONSE_TIMEOUT`],
    /// held open and never used again: the service releases every claim a
    /// connection holds once it closes, and frames this helper still reads
    /// were claimed on them.
    #[cfg(target_os = "linux")]
    surface_share_connections_set_aside_after_a_timeout: Mutex<Vec<UnixStream>>,
    /// One Vulkan device per child, created at first import.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    consumer_vulkan_device: Mutex<Option<Arc<ConsumerVulkanDevice>>>,
    /// Device exports memoised per surface id: the CUDA import and the
    /// timeline import are per-surface setup costs, never per-frame ones.
    ///
    /// Keyed by the source surface's id and held for this child's
    /// lifetime. The parent can evict a staging (its surface was
    /// unregistered), and this side cannot observe that — the next
    /// refill's escalate round trip fails by name instead, which is the
    /// honest answer to a surface that is gone.
    #[cfg(target_os = "linux")]
    device_exports_by_surface: Mutex<std::collections::HashMap<String, Arc<HelperDeviceExport>>>,
    /// CPU-readback exports memoised on the same key, for the same reason:
    /// the checkout and the Vulkan import are per-pool-slot setup costs,
    /// never per-frame ones.
    ///
    /// Its own map rather than a shared one because a surface can be
    /// reached through both doors in one helper process, and the two are
    /// different imports of two stagings the engine keeps at two
    /// residencies.
    #[cfg(target_os = "linux")]
    cpu_readback_exports_by_pool_slot:
        Mutex<std::collections::HashMap<String, Arc<HelperCpuReadbackExport>>>,
    /// The engine's write-back answer memoised per pool slot, seeded by
    /// whichever door asks first — both exports carry the same mint-time
    /// answer — so no two doors disagree about one frame.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    write_back_answers_by_pool_slot: Mutex<std::collections::HashMap<String, bool>>,
}

impl HelperProcessGpuExchangeClient {
    /// `surface_share_channel_name` is the socket path on Linux and the
    /// Mach service name on macOS — what the parent put in the helper's
    /// environment.
    pub(crate) fn new(
        escalate_request_to_parent: Py<PyAny>,
        release_to_parent_without_waiting: Py<PyAny>,
        surface_share_channel_name: impl Into<std::ffi::OsString>,
        foreign_surface_registration_runtime_id: String,
    ) -> Self {
        let surface_share_channel_name: std::ffi::OsString = surface_share_channel_name.into();
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let _ = surface_share_channel_name;
        Self {
            escalate_request_to_parent,
            release_to_parent_without_waiting,
            #[cfg(target_os = "linux")]
            surface_socket_path: PathBuf::from(surface_share_channel_name),
            #[cfg(target_os = "macos")]
            surface_share_mach_service_name: surface_share_channel_name,
            #[cfg(target_os = "macos")]
            surface_share_mach_connection: Mutex::new(None),
            #[cfg(target_os = "macos")]
            iosurface_imports_by_pool_slot: HelperIOSurfaceImportsByPoolSlot::default(),
            foreign_surface_registration_runtime_id,
            #[cfg(target_os = "linux")]
            surface_share_connection: Mutex::new(None),
            #[cfg(target_os = "linux")]
            surface_share_connections_set_aside_after_a_timeout: Mutex::new(Vec::new()),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            consumer_vulkan_device: Mutex::new(None),
            #[cfg(target_os = "linux")]
            device_exports_by_surface: Mutex::new(std::collections::HashMap::new()),
            #[cfg(target_os = "linux")]
            cpu_readback_exports_by_pool_slot: Mutex::new(std::collections::HashMap::new()),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            write_back_answers_by_pool_slot: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Ask the parent to allocate, then check the result out and import it.
    ///
    /// Called attached; the escalate wait releases the GIL, and the checkout
    /// and Vulkan import run detached.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn acquire_pixel_buffer(
        self: &Arc<Self>,
        python: Python<'_>,
        width: u32,
        height: u32,
        wire_format_name: &str,
    ) -> PyResult<HelperCheckedOutPixelSurface> {
        let op = PyDict::new(python);
        op.set_item("op", "acquire_pixel_buffer")?;
        op.set_item("width", width)?;
        op.set_item("height", height)?;
        op.set_item("format", wire_format_name)?;
        let response =
            escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        let handle_id: String = response
            .get_item("handle_id")
            .map_err(|_| {
                PyRuntimeError::new_err(
                    "the parent's acquire_pixel_buffer response carried no handle_id",
                )
            })?
            .extract()?;
        // The debt exists from the moment the parent allocated: if the
        // checkout or the Vulkan import below fails, this drops on the error
        // path and pays the `release_handle`, instead of stranding the
        // parent's pool slot and surface-share entry until teardown.
        let release_to_parent = HelperSurfaceReleaseDebt {
            release_to_parent_without_waiting: self
                .release_to_parent_without_waiting
                .clone_ref(python),
            handle_id: handle_id.clone(),
        };
        let checked_out = python.detach(|| self.check_out_and_import(&handle_id))?;
        let HelperCheckedOutSurface::PixelBuffer(mut checked_out_pixel_surface) = checked_out
        else {
            return Err(PyRuntimeError::new_err(format!(
                "acquire_pixel_buffer's allocation {handle_id:?} resolved to a texture \
                 registration; a pool cannot answer a buffer acquire with an image"
            )));
        };
        checked_out_pixel_surface.release_to_parent = Some(release_to_parent);
        Ok(checked_out_pixel_surface)
    }

    /// Check out a surface another processor published — pixel buffer or
    /// texture, whichever its registration names. No release debt: the
    /// surface belongs to its acquirer.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn resolve_surface(
        self: &Arc<Self>,
        python: Python<'_>,
        surface_id: &str,
    ) -> PyResult<HelperCheckedOutSurface> {
        python.detach(|| self.check_out_and_import(surface_id))
    }

    /// Claim a published surface against producer reuse, without importing
    /// its memory.
    ///
    /// The cheap half of [`Self::resolve_surface`]: the checkout is what mints
    /// the lease, and a holder that only needs the frame to hold still owes no
    /// Vulkan import for it. The plane fds the service delivers alongside the
    /// claim close with this call.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn claim_surface_against_producer_reuse(
        self: &Arc<Self>,
        surface_id: &str,
    ) -> PyResult<HelperSurfaceCheckOutLeaseDebt> {
        let (response, _transferred_handles_released_by_scope) =
            self.check_out_surface(surface_id)?;
        refuse_check_out_the_service_declined(format_args!("{surface_id:?}"), &response)?;
        Ok(HelperSurfaceCheckOutLeaseDebt {
            exchange_client: Arc::clone(self),
            surface_id: surface_id.to_string(),
        })
    }

    /// `check_out` over the surface-share socket, then the import of
    /// whichever backing the registration names.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn check_out_and_import(
        self: &Arc<Self>,
        surface_id: &str,
    ) -> PyResult<HelperCheckedOutSurface> {
        let (response, received_handles) = self.check_out_surface(surface_id)?;
        self.import_checked_out_surface(surface_id, &response, received_handles)
    }

    /// Wait for the parent's GPU device to go idle.
    ///
    /// A real wait, not an acknowledgement: the parent runs it inside its
    /// escalate scope, so the reply means the device was idle on that
    /// side — which is the only side there is.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn wait_device_idle(&self, python: Python<'_>) -> PyResult<()> {
        let op = PyDict::new(python);
        op.set_item("op", "wait_device_idle")?;
        escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        Ok(())
    }

    /// Claim a surface against producer reuse and take the handles its
    /// memory crosses as.
    ///
    /// The one place this op is spelled: a checkout is what pins the frame,
    /// and every caller owes the matching [`Self::release_check_out`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn check_out_surface(&self, surface_id: &str) -> PyResult<SurfaceShareAnswer> {
        self.surface_share_request(&serde_json::json!({
            "op": "check_out",
            "surface_id": surface_id,
        }))
    }

    /// Let go of one claim on a surface, freeing its slot for its producer.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn release_check_out(&self, surface_id: &str) -> PyResult<SurfaceShareAnswer> {
        self.surface_share_request(&serde_json::json!({
            "op": "release_check_out",
            "surface_id": surface_id,
        }))
    }

    /// Publish the layout this side left a texture in, so the next
    /// consumer's acquire barrier names the right source layout.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn publish_image_layout_to_surface_share(
        &self,
        surface_id: &str,
        current_image_layout_raw: i32,
    ) -> PyResult<()> {
        let (response, _no_handles) = self.surface_share_request(&serde_json::json!({
            "op": "update_layout",
            "surface_id": surface_id,
            "current_image_layout": current_image_layout_raw,
        }))?;
        if let Some(publish_error) = response.get("error").and_then(|value| value.as_str()) {
            return Err(PyRuntimeError::new_err(format!(
                "the surface-share service refused the layout publish for {surface_id:?}: \
                 {publish_error}"
            )));
        }
        match response.get("success").and_then(|value| value.as_bool()) {
            Some(true) => Ok(()),
            Some(false) => Err(PyRuntimeError::new_err(format!(
                "the surface-share service did not record the layout publish for \
                 {surface_id:?} — it knows no such registration"
            ))),
            None => Err(PyRuntimeError::new_err(format!(
                "the surface-share service's layout-publish answer for {surface_id:?} \
                 carried no success field"
            ))),
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn consumer_vulkan_device(&self) -> PyResult<Arc<ConsumerVulkanDevice>> {
        let mut device = self.consumer_vulkan_device.lock();
        if let Some(existing_device) = device.as_ref() {
            return Ok(Arc::clone(existing_device));
        }
        let created = Arc::new(ConsumerVulkanDevice::new().map_err(|device_failure| {
            PyRuntimeError::new_err(format!(
                "this helper process could not create its Vulkan import device: {device_failure}"
            ))
        })?);
        *device = Some(Arc::clone(&created));
        Ok(created)
    }
}

/// The lease against a real surface-share service.
///
/// The RAII floor of the lifetime contract: a checkout claims the frame, and
/// the debt's drop releases it. Provable without a GPU — the wheel's device
/// tests are `requires_gpu` and CI declares no GPU runner, so the lease's
/// balance has to hold here or it is not protected anywhere.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod surface_check_out_lease_debt_tests;
