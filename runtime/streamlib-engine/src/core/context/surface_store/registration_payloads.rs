// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::os::fd::{FromRawFd as _, OwnedFd};

use crate::core::context::surface_share_wire_verbs::{
    SURFACE_RESOURCE_TYPE_PIXEL_BUFFER, SURFACE_RESOURCE_TYPE_STORAGE_BUFFER,
    SURFACE_RESOURCE_TYPE_TEXTURE,
};
use crate::core::context::surface_share_wire_verbs::{
    SURFACE_WIRE_FIELD_EXPORTING_DEVICE_UUID, SURFACE_WIRE_FIELD_VK_MEMORY_TYPE_INDEX,
};
use crate::core::rhi::PixelBuffer;
use crate::core::rhi::PixelFormat;
use crate::core::{Error, Result};

use super::{SURFACE_HANDLE_TYPE_DMA_BUF, SURFACE_HANDLE_TYPE_OPAQUE_FD};

/// Reply flag announcing a `produce_done` timeline edge appended after the
/// plane fds.
pub(super) const SURFACE_REPLY_HAS_PRODUCE_DONE_FD: &str = "has_produce_done_fd";

/// Reply flag announcing a `consume_done` timeline edge appended after the
/// plane fds (and after `produce_done` when both are present).
pub(super) const SURFACE_REPLY_HAS_CONSUME_DONE_FD: &str = "has_consume_done_fd";

/// Every plane of a pixel buffer exported for the surface-share wire: the
/// fds to attach, the metadata arrays describing them, and the one flavour
/// the checkout importer dispatches on.
///
/// The plane fds are owned, never observed: an exporter mints a fresh fd per
/// call and surrenders it, so adoption here is what closes each one exactly
/// once — on the send, on a refusal, and on every early return in between.
pub(super) struct ExportedPlaneWireHandles {
    pub(super) plane_fds: Vec<OwnedFd>,
    pub(super) plane_sizes: Vec<u64>,
    pub(super) plane_offsets: Vec<u64>,
    pub(super) plane_flavour: ExportedPlaneFlavourOnTheWire,
}

/// The external-handle flavour every plane of one export shares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ExportedPlaneFlavourOnTheWire {
    DmaBuf,
    OpaqueFd(OpaqueFdExportContract),
}

impl ExportedPlaneFlavourOnTheWire {
    /// The `handle_type` wire value this flavour registers under.
    fn wire_handle_type(&self) -> &'static str {
        match self {
            Self::DmaBuf => SURFACE_HANDLE_TYPE_DMA_BUF,
            Self::OpaqueFd(_) => SURFACE_HANDLE_TYPE_OPAQUE_FD,
        }
    }
}

/// What an OPAQUE_FD registration states beside its fd, because a
/// conforming importer cannot derive either from the fd: the handle type has
/// no fd-properties query, and it is bound to the device that exported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OpaqueFdExportContract {
    pub(super) vk_memory_type_index: u32,
    pub(super) exporting_device_uuid: String,
}

/// Export every plane of `pixel_buffer` for a surface-share registration.
///
/// Single-plane pixel buffers return a one-element vec; multi-plane
/// DMA-BUFs (e.g. NV12 under DRM format modifiers) one fd per plane;
/// OPAQUE_FD-flavored buffers (a device that exports no DMA-BUF, CUDA
/// targets) a single OPAQUE_FD handle.
pub(super) fn exported_plane_wire_handles(
    pixel_buffer: &PixelBuffer,
) -> Result<ExportedPlaneWireHandles> {
    use crate::core::rhi::RhiPixelBufferExport;

    let exporting_device_uuid = pixel_buffer
        .buffer_ref()
        .inner
        .vulkan_device()
        .physical_device_uuid();
    adopt_exported_planes_into_wire_handles(
        pixel_buffer.export_plane_handles()?,
        exporting_device_uuid,
    )
}

/// Write the OPAQUE_FD export contract into a pixel-buffer registration or
/// check-in `payload`; a DMA-BUF payload is left as it was.
fn state_the_opaque_fd_export_contract_of_the_planes(
    payload: &mut serde_json::Value,
    exported_planes: &ExportedPlaneWireHandles,
) {
    if let ExportedPlaneFlavourOnTheWire::OpaqueFd(opaque_fd_export_contract) =
        &exported_planes.plane_flavour
    {
        payload[SURFACE_WIRE_FIELD_VK_MEMORY_TYPE_INDEX] =
            opaque_fd_export_contract.vk_memory_type_index.into();
        payload[SURFACE_WIRE_FIELD_EXPORTING_DEVICE_UUID] = opaque_fd_export_contract
            .exporting_device_uuid
            .clone()
            .into();
    }
}

/// The `check_in` payload for a pixel buffer whose planes were exported as
/// `exported_planes`.
pub(super) fn pixel_buffer_check_in_payload(
    runtime_id: &str,
    pixel_buffer_shape: PixelBufferShapeOnTheWire,
    exported_planes: &ExportedPlaneWireHandles,
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "op": "check_in",
        "runtime_id": runtime_id,
        "width": pixel_buffer_shape.width,
        "height": pixel_buffer_shape.height,
        "format": pixel_buffer_shape.format.wire_name(),
        "handle_type": exported_planes.plane_flavour.wire_handle_type(),
        "plane_sizes": exported_planes.plane_sizes,
        "plane_offsets": exported_planes.plane_offsets,
    });
    state_the_opaque_fd_export_contract_of_the_planes(&mut payload, exported_planes);
    payload
}

/// The extent and format a pixel-buffer registration or check-in states.
#[derive(Debug, Clone, Copy)]
pub(super) struct PixelBufferShapeOnTheWire {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) format: PixelFormat,
}

impl PixelBufferShapeOnTheWire {
    pub(super) fn of(pixel_buffer: &PixelBuffer) -> Self {
        Self {
            width: pixel_buffer.width,
            height: pixel_buffer.height,
            format: pixel_buffer.format(),
        }
    }
}

/// Which timeline edges a pixel-buffer registration announces it carries.
#[derive(Debug, Clone, Copy)]
pub(super) struct AnnouncedTimelineEdges {
    pub(super) has_produce_done_fd: bool,
    pub(super) has_consume_done_fd: bool,
}

/// The `register` payload for a pixel buffer whose planes were exported as
/// `exported_planes`; `announced_timeline_edges` is `None` for a registration
/// that carries no timeline at all.
pub(super) fn pixel_buffer_registration_payload(
    surface_id: &str,
    runtime_id: &str,
    pixel_buffer_shape: PixelBufferShapeOnTheWire,
    exported_planes: &ExportedPlaneWireHandles,
    announced_timeline_edges: Option<AnnouncedTimelineEdges>,
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "op": "register",
        "surface_id": surface_id,
        "runtime_id": runtime_id,
        "width": pixel_buffer_shape.width,
        "height": pixel_buffer_shape.height,
        "format": pixel_buffer_shape.format.wire_name(),
        "resource_type": SURFACE_RESOURCE_TYPE_PIXEL_BUFFER,
        "handle_type": exported_planes.plane_flavour.wire_handle_type(),
        "plane_sizes": exported_planes.plane_sizes,
        "plane_offsets": exported_planes.plane_offsets,
    });
    if let Some(announced_timeline_edges) = announced_timeline_edges {
        payload[SURFACE_REPLY_HAS_PRODUCE_DONE_FD] =
            announced_timeline_edges.has_produce_done_fd.into();
        payload[SURFACE_REPLY_HAS_CONSUME_DONE_FD] =
            announced_timeline_edges.has_consume_done_fd.into();
    }
    state_the_opaque_fd_export_contract_of_the_planes(&mut payload, exported_planes);
    payload
}

/// Adopt one export's worth of plane handles into the wire shape.
///
/// One registration carries one flavour — a buffer whose planes export under
/// two is refused rather than published under whichever flavour happened to
/// come last, which would import the other planes through the wrong Vulkan
/// external-handle type. For the same reason OPAQUE_FD planes stating
/// different memory type indices are refused: the registration states one.
///
/// Every `exported_planes` fd must come from an export that minted it for
/// this process: they are adopted, not borrowed. `RhiExternalHandle` also
/// carries kernel-delivered fds on the import side (see
/// [`external_plane_handles_for_flavour`]), and those must never reach here.
/// `exporting_device_uuid` is the device the planes' memory lives on, stated
/// on an OPAQUE_FD registration.
pub(super) fn adopt_exported_planes_into_wire_handles(
    exported_planes: Vec<crate::core::rhi::RhiExternalHandle>,
    exporting_device_uuid: [u8; 16],
) -> Result<ExportedPlaneWireHandles> {
    use crate::core::rhi::RhiExternalHandle;

    let exporting_device_uuid =
        streamlib_consumer_rhi::lowercase_hex_of_vulkan_device_uuid(exporting_device_uuid);
    let mut plane_fds: Vec<OwnedFd> = Vec::with_capacity(exported_planes.len());
    let mut plane_sizes: Vec<u64> = Vec::with_capacity(exported_planes.len());
    let mut plane_offsets: Vec<u64> = Vec::with_capacity(exported_planes.len());
    let mut plane_flavours: Vec<ExportedPlaneFlavourOnTheWire> =
        Vec::with_capacity(exported_planes.len());
    for exported_plane in exported_planes {
        let (exported_plane_fd, size, this_plane_flavour) = match exported_plane {
            RhiExternalHandle::DmaBuf { fd, size } => {
                (fd, size, ExportedPlaneFlavourOnTheWire::DmaBuf)
            }
            RhiExternalHandle::OpaqueFd {
                fd,
                size,
                exporting_allocation_memory_type_index,
            } => (
                fd,
                size,
                ExportedPlaneFlavourOnTheWire::OpaqueFd(OpaqueFdExportContract {
                    vk_memory_type_index: exporting_allocation_memory_type_index,
                    exporting_device_uuid: exporting_device_uuid.clone(),
                }),
            ),
        };
        // SAFETY: the export minted this fd for us and handed it to no one
        // else.
        plane_fds.push(unsafe { OwnedFd::from_raw_fd(exported_plane_fd) });
        plane_sizes.push(size as u64);
        plane_offsets.push(0);
        plane_flavours.push(this_plane_flavour);
    }
    // Validated after every fd is adopted, so a refusal closes all of them
    // rather than only the ones the loop reached.
    let plane_flavour = plane_flavours
        .first()
        .cloned()
        .unwrap_or(ExportedPlaneFlavourOnTheWire::DmaBuf);
    if plane_flavours.iter().any(|this_plane_flavour| {
        this_plane_flavour.wire_handle_type() != plane_flavour.wire_handle_type()
    }) {
        let plane_handle_types: Vec<&'static str> = plane_flavours
            .iter()
            .map(ExportedPlaneFlavourOnTheWire::wire_handle_type)
            .collect();
        return Err(Error::Configuration(format!(
            "pixel buffer exports mixed external-handle flavours ({plane_handle_types:?})"
        )));
    }
    if plane_flavours
        .iter()
        .any(|this_plane_flavour| *this_plane_flavour != plane_flavour)
    {
        let plane_memory_type_indices: Vec<u32> = plane_flavours
            .iter()
            .filter_map(|this_plane_flavour| match this_plane_flavour {
                ExportedPlaneFlavourOnTheWire::OpaqueFd(contract) => {
                    Some(contract.vk_memory_type_index)
                }
                ExportedPlaneFlavourOnTheWire::DmaBuf => None,
            })
            .collect();
        return Err(Error::Configuration(format!(
            "pixel buffer exports OPAQUE_FD planes at different memory type indices \
             ({plane_memory_type_indices:?}); one registration states one"
        )));
    }
    Ok(ExportedPlaneWireHandles {
        plane_fds,
        plane_sizes,
        plane_offsets,
        plane_flavour,
    })
}

/// Export one timeline-semaphore edge as the OPAQUE_FD the wire carries.
///
/// The host keeps the semaphore object; the kernel duplicates the fd during
/// SCM_RIGHTS, so this copy is ours alone and closes with the scope that
/// adopted it.
pub(super) fn exported_timeline_edge_opaque_fd(
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

/// Wrap each received fd as the external-handle flavour the surface-share
/// service registered it under.
///
/// The two flavours are distinct Vulkan external-handle types. Importing an
/// OPAQUE_FD fd through the DMA-BUF type does not fail cleanly — it hands the
/// driver a handle of the wrong type — so the wire's discriminator is honoured
/// rather than assumed. An OPAQUE_FD surface must state the exporter's
/// `vk_memory_type_index`, the only index a conforming import may bind; one
/// that does not is refused by name.
pub(super) fn external_plane_handles_for_flavour(
    operation: &str,
    handle_type: &str,
    received_fds: &[std::os::fd::RawFd],
    plane_sizes: &[u64],
    stated_vk_memory_type_index: Option<u32>,
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
        SURFACE_HANDLE_TYPE_OPAQUE_FD => {
            let exporting_allocation_memory_type_index =
                stated_vk_memory_type_index.ok_or_else(|| {
                    Error::Configuration(format!(
                        "{operation}: an OPAQUE_FD surface was registered without the exporter's \
                         vk_memory_type_index, so no conforming import can bind it"
                    ))
                })?;
            Ok(received_fds
                .iter()
                .zip(plane_sizes.iter())
                .map(|(fd, size)| RhiExternalHandle::OpaqueFd {
                    fd: *fd,
                    size: *size as usize,
                    exporting_allocation_memory_type_index,
                })
                .collect())
        }
        unknown => Err(Error::Configuration(format!(
            "{operation}: surface registered with unknown handle type {unknown:?}; the wire \
             carries {SURFACE_HANDLE_TYPE_DMA_BUF:?} or {SURFACE_HANDLE_TYPE_OPAQUE_FD:?}"
        ))),
    }
}

/// The memory type index an OPAQUE_FD registration must publish, or a
/// refusal naming what had no allocation to read one from.
///
/// One rule for every OPAQUE_FD export: the handle type has no
/// `vkGetMemoryFdPropertiesKHR` query, so an importer that is not told
/// the index guesses, and a guess that happens to bind is the two sides
/// coincidentally landing on the same memory type.
pub(super) fn memory_type_index_stated_by_an_opaque_fd_export(
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
pub(super) fn surface_export_staging_registration_payload(
    surface_id: &str,
    runtime_id: &str,
    staging_byte_size: u64,
    width: u32,
    height: u32,
    format: PixelFormat,
    memory_type_index: u32,
    exporting_device_uuid: &str,
    refill_done_timeline_crosses: bool,
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
        SURFACE_REPLY_HAS_PRODUCE_DONE_FD: refill_done_timeline_crosses,
        SURFACE_REPLY_HAS_CONSUME_DONE_FD: false,
        SURFACE_WIRE_FIELD_VK_MEMORY_TYPE_INDEX: memory_type_index,
        SURFACE_WIRE_FIELD_EXPORTING_DEVICE_UUID: exporting_device_uuid,
    })
}

/// The surface-share registration payload for a tensor storage buffer.
pub(super) fn storage_buffer_registration_payload(
    surface_id: &str,
    runtime_id: &str,
    tensor_layout: &crate::core::rhi::TensorStorageBufferLayout,
    memory_type_index: u32,
    exporting_device_uuid: &str,
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "op": "register",
        "surface_id": surface_id,
        "runtime_id": runtime_id,
        "resource_type": SURFACE_RESOURCE_TYPE_STORAGE_BUFFER,
        "handle_type": SURFACE_HANDLE_TYPE_OPAQUE_FD,
        "plane_sizes": [tensor_layout.byte_size()],
        "plane_offsets": [0u64],
        "plane_strides": [0u64],
        SURFACE_WIRE_FIELD_VK_MEMORY_TYPE_INDEX: memory_type_index,
        SURFACE_WIRE_FIELD_EXPORTING_DEVICE_UUID: exporting_device_uuid,
    });
    tensor_layout.write_surface_share_fields(&mut payload);
    payload
}

/// The `register` request for an OPAQUE_FD texture: the memory as one
/// plane, the `vk_image_*` shape
/// [`crate::vulkan::rhi::HostVulkanTexture::new_opaque_fd_export`] fixes with
/// the usage the image was created with, and the exporter's memory type index
/// and device UUID.
pub(super) fn opaque_fd_texture_registration_payload(
    texture: &crate::vulkan::rhi::HostVulkanTexture,
    surface_id: &str,
    runtime_id: &str,
    has_produce_done_fd: bool,
    has_consume_done_fd: bool,
    current_image_layout: streamlib_consumer_rhi::VulkanLayout,
) -> Result<serde_json::Value> {
    use crate::vulkan::rhi::VulkanTextureLike as _;
    const VK_IMAGE_TYPE_2D: i32 = 1;
    const VK_IMAGE_TILING_OPTIMAL: i32 = 0;
    const VK_SAMPLE_COUNT_1: i32 = 1;

    let allocation_size = texture.vma_allocation_size();
    let memory_type_index = memory_type_index_stated_by_an_opaque_fd_export(
        texture.vma_allocation_memory_type_index(),
        surface_id,
        "OPAQUE_FD texture registration",
    )?;
    let exporting_device_uuid = streamlib_consumer_rhi::lowercase_hex_of_vulkan_device_uuid(
        texture.exporting_physical_device_uuid().ok_or_else(|| {
            Error::GpuError(format!(
                "OPAQUE_FD texture registration for {surface_id:?} has no stored device to read \
                 the exporting device UUID from; an import on the wrong GPU corrupts silently \
                 instead of failing"
            ))
        })?,
    );
    Ok(serde_json::json!({
        "op": "register",
        "surface_id": surface_id,
        "runtime_id": runtime_id,
        "width": texture.width(),
        "height": texture.height(),
        "format": texture.format().wire_name(),
        "resource_type": SURFACE_RESOURCE_TYPE_TEXTURE,
        "handle_type": SURFACE_HANDLE_TYPE_OPAQUE_FD,
        "plane_sizes": [allocation_size],
        "plane_offsets": [0u64],
        "plane_strides": [0u64],
        "drm_format_modifier": 0u64,
        SURFACE_REPLY_HAS_PRODUCE_DONE_FD: has_produce_done_fd,
        SURFACE_REPLY_HAS_CONSUME_DONE_FD: has_consume_done_fd,
        "current_image_layout": current_image_layout.as_vk().as_raw(),
        "vk_image_type": VK_IMAGE_TYPE_2D,
        "vk_image_mip_levels": 1u32,
        "vk_image_array_layers": 1u32,
        "vk_image_samples": VK_SAMPLE_COUNT_1,
        "vk_image_tiling": VK_IMAGE_TILING_OPTIMAL,
        "vk_image_usage": texture.vk_image_usage_flags().bits(),
        "vk_image_allocation_size": allocation_size,
        SURFACE_WIRE_FIELD_VK_MEMORY_TYPE_INDEX: memory_type_index,
        SURFACE_WIRE_FIELD_EXPORTING_DEVICE_UUID: exporting_device_uuid,
    }))
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
/// NVIDIA EGL's sampler-only linear DMA-BUFs).
pub(super) fn dma_buf_texture_registration_payload(
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
    // A one-plane image's plane spans its whole allocation, which a display
    // importer (Chromium's native pixmap) is handed as the plane's size. A
    // multi-plane image states no per-plane size, so its planes carry none.
    let plane_sizes: Vec<u64> = match plane_layout.len() {
        1 => vec![texture.vma_allocation_size()],
        plane_count => vec![0; plane_count],
    };

    serde_json::json!({
        "op": "register",
        "surface_id": surface_id,
        "runtime_id": runtime_id,
        "width": texture.width(),
        "height": texture.height(),
        "format": texture.format().wire_name(),
        "resource_type": SURFACE_RESOURCE_TYPE_TEXTURE,
        "plane_sizes": plane_sizes,
        "plane_offsets": plane_offsets,
        "plane_strides": plane_strides,
        "drm_format_modifier": texture.chosen_drm_format_modifier(),
        "vk_image_tiling": texture.vk_image_tiling().as_raw(),
        // The host allocation's byte size, which a consumer-side
        // `import_render_target_dma_buf` must pass to `vkAllocateMemory` —
        // deriving it from extent × stride under-sizes tiled allocations and
        // fails the bind.
        "vk_image_allocation_size": texture.vma_allocation_size(),
        SURFACE_REPLY_HAS_PRODUCE_DONE_FD: has_produce_done_fd,
        SURFACE_REPLY_HAS_CONSUME_DONE_FD: has_consume_done_fd,
        // The producer's declared `VkImageLayout`: the layout the texture
        // lives in immediately after registration, fed to host consumers as
        // the source layout of their first QFOT acquire barrier. Encoded as
        // i32 per the Vulkan spec.
        "current_image_layout": current_image_layout.as_vk().as_raw(),
    })
}
