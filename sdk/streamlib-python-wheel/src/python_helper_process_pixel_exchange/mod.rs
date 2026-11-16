// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The helper child's half of the pixel exchange.
//!
//! A Python processor's pixels live in the engine's pools, one process away.
//! Acquiring goes through two channels that already exist: the escalate
//! socket carries the request (the parent allocates, checks the buffer into
//! its surface-share service, and answers with the minted surface id), and
//! the surface-share socket carries the memory itself — the checkout's
//! `recvmsg` lands the memory fds in this process's fd table via
//! SCM_RIGHTS, and the consumer-side Vulkan import maps them. The escalate
//! socket never carries fds.
//!
//! Pool allocations are DMA-BUF-flavoured where the device exports
//! DMA-BUF and OPAQUE_FD where it does not — the software device — and a
//! pixel buffer of either flavour imports and maps here. External device
//! APIs import OPAQUE_FD, and one allocation cannot export both on NVIDIA. So
//! the device side goes through the parent's per-surface staging buffer:
//! `open_device_export_staging` has the parent publish that staging and
//! its refill timeline, the checkout delivers both fds, CUDA imports the
//! memory, and each refill is an escalate round trip whose answer is the
//! timeline value to wait for. The host's own wait after a refill orders
//! nothing for this process; the timeline does.

use pyo3::prelude::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::sync::Arc;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use parking_lot::Mutex;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use streamlib_consumer_rhi::ConsumerVulkanDevice;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod checked_out_surface;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod foreign_surface_adoption;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod gpu_kernels;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod parent_and_surface_share_round_trips;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod pooled_gpu_frame_claims;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod storage_buffer;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod surface_color_conversion;
mod surface_copy;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod vk_image_creation_recipe_wire_parse;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use checked_out_surface::{
    HelperCheckedOutPixelSurface, HelperCheckedOutSurface, HelperSurfaceCheckOutLeaseDebt,
    HelperSurfaceHeldAgainstProducerReuse, HelperSurfaceReleaseDebt,
    a_tensor_surface_is_not_a_pixel_surface,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use parent_and_surface_share_round_trips::{
    escalate_round_trip_to_parent, hand_a_release_handle_to_the_release_worker,
    refuse_check_out_the_service_declined, required_positive_u32_check_out_metadata_field,
    response_field,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use vk_image_creation_recipe_wire_parse::vk_image_creation_recipe_of_check_out;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use gpu_kernels::{
    HelperProcessGraphicsDraw, HelperProcessGraphicsKernelRegistration,
    HelperProcessRayTracingKernelRegistration, compute_dispatch_wire_entry,
};

#[cfg(target_os = "linux")]
pub(crate) use linux::{
    CpuReadbackCopyDirection, DRM_FORMAT_MOD_LINEAR, ForeignDmaBufTextureWithATiledModifier,
    HelperAcquiredTexture, HelperCheckedOutStorageBuffer, HelperCheckedOutTextureSurface,
    HelperCpuReadbackExport, HelperDeviceExport, HelperForeignSurfaceUnregisterDebt,
    OpaqueFdAllocationExportDescription, foreign_dma_buf_plane_layout,
};
#[cfg(target_os = "linux")]
use linux::{
    HelperPixelBufferPoolSlotImport, HelperPoolSlotImport,
    surface_share_connection_over_the_channel_named,
};
#[cfg(target_os = "macos")]
pub(crate) use macos::{
    HelperCheckedOutStorageBuffer, HelperCheckedOutTextureSurface,
    IOSurfaceMachPortExportDescription,
};
#[cfg(target_os = "macos")]
use macos::{
    HelperIOSurfaceCpuLock, HelperIOSurfacePixelBufferPoolSlotImport, HelperIOSurfaceUseCountClaim,
    HelperPoolSlotImport, surface_share_connection_over_the_channel_named,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use streamlib_gpu_frame_import_client::GpuPoolSlotFrameClaimOfThisImporter;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use streamlib_gpu_frame_import_client::{
    GpuPoolClaimTablesOfThisImporter, SurfaceShareConnectionOfThisImporter,
};

/// The width, height and format a pixel surface carries.
#[derive(Clone, Copy)]
pub(crate) struct PixelSurfaceGeometry {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format_wire_name: &'static str,
}

/// What a surface is shaped as: pixels, or a tensor storage buffer's declared
/// shape and dtype — never both, never neither.
#[derive(Clone)]
pub(crate) enum GpuSurfaceGeometry {
    Pixels(PixelSurfaceGeometry),
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    TensorStorageBuffer(streamlib::sdk::rhi::TensorStorageBufferLayout),
}

/// Which processor output pool a texture or tensor storage buffer acquire
/// hands its slot out of.
#[derive(Clone, Copy)]
pub(crate) struct ProcessorOutputPoolRequest<'pool_key> {
    pub(crate) pool_key: &'pool_key str,
    pub(crate) rotation_depth: u32,
}

/// The child-side client that fulfills `ctx.gpu_limited_access` calls by
/// crossing to the parent: escalate for allocation, surface-share for the
/// memory, one consumer Vulkan device per child for the import.
pub(crate) struct HelperProcessGpuExchangeClient {
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        expect(dead_code, reason = "only the Linux and macOS builds use it")
    )]
    escalate_request_to_parent: Py<PyAny>,
    /// The bridge's door for a release that must not wait on its answer where
    /// it is owed — every release a drop owes.
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        expect(dead_code, reason = "only the Linux and macOS builds use it")
    )]
    release_to_parent_without_waiting: Py<PyAny>,
    /// The connection to the runtime's surface-sharing service — its socket
    /// on Linux, its Mach service on macOS — and the pool slot imports made
    /// over it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    surface_share_connection: SurfaceShareConnectionOfThisImporter<HelperPoolSlotImport>,
    /// The runtime id this client's foreign-surface adoptions register and
    /// release under. Deliberately **not** the node's own runtime id: the
    /// service's crash watchdog releases every surface a disconnected
    /// subprocess registered *by runtime id*, so a child registering under
    /// the node's id would have its crash sweep the parent's own
    /// registrations. A child-scoped id makes the sweep exactly this
    /// child's adoptions.
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        expect(dead_code, reason = "only the Linux and macOS builds use it")
    )]
    foreign_surface_registration_runtime_id: String,
    /// The claim tables this helper was handed, mapped once per pool, and the
    /// claims its frames hold in them.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    gpu_pool_claim_tables: GpuPoolClaimTablesOfThisImporter,
    /// The `deviceUUID` the runtime named at spawn, or why this child has
    /// none — what its import device is built on, and the refusal of every
    /// import when it is absent.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    vulkan_device_uuid_the_runtime_named_at_spawn: streamlib_consumer_rhi::Result<[u8; 16]>,
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
    /// Each foreign DMA-BUF this helper adopted, with whether a
    /// `convert_surface_into` named it as its source — the parent then holds
    /// what it resolved until the adoption's release says to drop it. Every
    /// other source is a surface whose own release already tells the parent.
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        expect(dead_code, reason = "only the Linux and macOS builds use it")
    )]
    foreign_adoptions_and_whether_a_conversion_named_them:
        Mutex<std::collections::HashMap<String, bool>>,
}

impl HelperProcessGpuExchangeClient {
    /// `surface_share_channel_name` is the socket path on Linux and the
    /// Mach service name on macOS, and `vulkan_device_uuid_the_runtime_named_at_spawn`
    /// the runtime's device — what the parent put in the helper's environment.
    pub(crate) fn new(
        escalate_request_to_parent: Py<PyAny>,
        release_to_parent_without_waiting: Py<PyAny>,
        surface_share_channel_name: impl Into<std::ffi::OsString>,
        vulkan_device_uuid_the_runtime_named_at_spawn: streamlib_consumer_rhi::Result<[u8; 16]>,
        foreign_surface_registration_runtime_id: String,
    ) -> Self {
        let surface_share_channel_name: std::ffi::OsString = surface_share_channel_name.into();
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let _ = (
            surface_share_channel_name,
            vulkan_device_uuid_the_runtime_named_at_spawn,
        );
        Self {
            escalate_request_to_parent,
            release_to_parent_without_waiting,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            surface_share_connection: surface_share_connection_over_the_channel_named(
                surface_share_channel_name,
            ),
            foreign_surface_registration_runtime_id,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            gpu_pool_claim_tables: GpuPoolClaimTablesOfThisImporter::default(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            vulkan_device_uuid_the_runtime_named_at_spawn,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            consumer_vulkan_device: Mutex::new(None),
            #[cfg(target_os = "linux")]
            device_exports_by_surface: Mutex::new(std::collections::HashMap::new()),
            #[cfg(target_os = "linux")]
            cpu_readback_exports_by_pool_slot: Mutex::new(std::collections::HashMap::new()),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            write_back_answers_by_pool_slot: Mutex::new(std::collections::HashMap::new()),
            foreign_adoptions_and_whether_a_conversion_named_them: Mutex::new(
                std::collections::HashMap::new(),
            ),
        }
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
