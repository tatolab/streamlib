// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use uuid::Uuid;

use super::super::handle_lifecycle::{EscalateHandleRegistry, RegisteredHandle};
use super::{new_exportable_timeline_edge, parse_texture_format};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestAcquireImage;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};
use crate::core::context::{
    GpuContextFullAccess, GpuContextLimitedAccess, OneOffAcquisitionShape, PooledTextureHandle,
    TextureCrossProcessImportability, TexturePoolDescriptor,
};
use crate::core::rhi::{TextureFormat, TextureUsages};
use crate::host_rhi::HostSurfaceStoreExt;

/// Allocate one texture and register it for a helper — with the
/// surface-share service and the parent's texture cache — answering the id it
/// is registered under and what holds it alive.
pub(super) fn allocate_registered_texture_for_helper(
    full: &GpuContextFullAccess,
    width: u32,
    height: u32,
    parsed_format: TextureFormat,
    parsed_usage: TextureUsages,
) -> crate::core::error::Result<(String, RegisteredHandle)> {
    // The importability flavor is derived engine-side from the request —
    // there is no Python dial for it, and a flavor the request cannot take
    // falls back to NotImportable so a later import refuses by name instead of
    // the acquire failing.
    let desc = TexturePoolDescriptor::new(width, height, parsed_format)
        .with_usage(parsed_usage)
        .with_cross_process_importability(match full.host_vulkan_device_arc() {
            Ok(device) => derive_texture_cross_process_importability(
                parsed_format,
                parsed_usage,
                TextureExportCapabilitiesOfTheDevice {
                    exports_dma_buf: device.supports_dma_buf_export(),
                    has_a_render_target_modifier_for_the_format: device
                        .has_render_target_modifier_for_texture_format(parsed_format),
                    has_the_opaque_fd_image_pool: device.opaque_fd_image_pool().is_some(),
                },
            ),
            Err(_) => TextureCrossProcessImportability::NotImportable,
        });
    let texture = full.host_inner().acquire_texture(&desc)?;
    let (handle_id, produce_done, consume_done) = assign_texture_handle_id(full, &texture)?;
    texture.note_a_surface_share_id_naming_its_pool_slot(&handle_id);
    // The parent answers its own binding resolutions from the texture cache —
    // without this entry it would re-import its own allocation through the
    // surface-share socket, a path that cannot rebuild every flavour and
    // re-interprets the ones it can.
    full.register_texture(&handle_id, texture.texture_clone());
    Ok((
        handle_id,
        RegisteredHandle::Texture {
            texture,
            produce_done,
            consume_done,
        },
    ))
}

/// Acquire a render-target DMA-BUF image on behalf of a helper process,
/// holding it and its timeline pair in `registry` until the helper releases
/// it; counted against the node's one-off acquisitions of its shape.
pub(in super::super) fn handle_acquire_image(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    rid: String,
    request: EscalateRequestAcquireImage,
) -> EscalateResponse {
    let EscalateRequestAcquireImage {
        request_id: _,
        width,
        height,
        format,
    } = request;
    let parsed_format = match parse_texture_format(&format) {
        Ok(f) => f,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: e,
            });
        }
    };
    // Render-target images carry their own usage signature —
    // they can be sampled, copied, AND used as render attachments.
    // The wire op deliberately does not take a usage list (that's
    // an acquire_texture concern); here the host knows the exact
    // set because the consumer is always a render-target adapter.
    let acquired = registry.acquire_a_one_off_for_the_helper_counted_under_its_shape(
        sandbox,
        OneOffAcquisitionShape::RenderTargetImage {
            width,
            height,
            format: parsed_format,
        },
        |full| {
            let texture = full.host_inner().acquire_render_target_dma_buf_image(
                width,
                height,
                parsed_format,
            )?;
            let (handle_id, produce_done, consume_done) = assign_image_handle_id(full, &texture)?;
            // The timeline pair, where the device exports one, is held with
            // the image so the FDs handed to surface-share stay valid for the
            // registration's lifetime.
            Ok((
                handle_id,
                RegisteredHandle::Image {
                    texture,
                    produce_done,
                    consume_done,
                },
            ))
        },
    );
    match acquired {
        Ok(handle_id) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            handle_id,
            width: Some(width),
            height: Some(height),
            format: Some(parsed_format.wire_name().to_string()),
            usage: Some(vec![
                "render_attachment".to_string(),
                "texture_binding".to_string(),
                "copy_src".to_string(),
            ]),
            ..Default::default()
        }),
        Err(e) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("acquire_image failed: {e}"),
        }),
    }
}

/// Resolve the `handle_id` returned to the subprocess for a pooled texture.
///
/// On Linux, register the texture's DMA-BUF with the surface-share service under a fresh UUID
/// so the subprocess can `check_out` it; on other platforms just mint a UUID.
///
/// On Linux, also allocates and registers a single-writer-per-edge
/// timeline pair (`produce_done` + `consume_done` per
/// `docs/architecture/adapter-timeline-single-writer.md`) where the device
/// exports a timeline semaphore; a device that exports none registers no
/// edge. The timelines are returned so the caller can stash them in the
/// [`EscalateHandleRegistry`]; the surface-share registration
/// duplicates the FDs via SCM_RIGHTS but the host-side Arcs must
/// outlive the registration.
pub(super) fn assign_texture_handle_id(
    full: &crate::core::context::GpuContextFullAccess,
    texture: &PooledTextureHandle,
) -> crate::core::error::Result<(
    String,
    Option<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>>,
    Option<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>>,
)> {
    let handle_id = Uuid::new_v4().to_string();
    if let Some(store) = full.surface_store() {
        let host_device = full.host_vulkan_device_arc()?;
        // Single-writer-per-edge timelines. Escalate-IPC consumers
        // (CPU-readback bridge today) handle sync via the
        // per-acquire response and don't drive these timelines,
        // but the surface-share IPC delivers both FDs to the
        // cdylib so future consumers riding the dual-timeline
        // contract see them.
        let (produce_done, consume_done) = timeline_edge_pair_where_the_device_exports_one(
            &host_device,
            "assign_texture_handle_id",
        )?
        .unzip();
        // UNDEFINED at registration: pooled textures sit in the
        // texture pool unowned until the first acquire. The host
        // adapter or escalate-IPC bridge transitions to its
        // workload-specific layout on first use; subsequent
        // releases publish the post-release layout via
        // `update_image_layout`.
        store.register_texture(
            &handle_id,
            texture.texture(),
            produce_done.as_deref(),
            consume_done.as_deref(),
            streamlib_consumer_rhi::VulkanLayout::UNDEFINED,
        )?;
        Ok((handle_id, produce_done, consume_done))
    } else {
        Ok((handle_id, None, None))
    }
}

/// Resolve the `handle_id` for a render-target DMA-BUF image.
///
/// On Linux, register the image's DMA-BUF (with the chosen DRM modifier and
/// per-plane row pitches) with the surface-share service under a fresh UUID
/// so the subprocess can `check_out` it; the surface-share registration
/// carries the modifier and strides the consumer-side EGL import requires.
///
/// Also allocates a single-writer-per-edge timeline pair, where the device
/// exports a timeline semaphore, and registers it with surface-share.
/// Returns the timelines so the caller can stash them in the
/// [`EscalateHandleRegistry`].
pub(super) fn assign_image_handle_id(
    full: &crate::core::context::GpuContextFullAccess,
    texture: &crate::core::rhi::Texture,
) -> crate::core::error::Result<(
    String,
    Option<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>>,
    Option<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>>,
)> {
    let handle_id = Uuid::new_v4().to_string();
    let host_device = full.host_vulkan_device_arc()?;
    let (produce_done, consume_done) =
        timeline_edge_pair_where_the_device_exports_one(&host_device, "assign_image_handle_id")?
            .unzip();
    if let Some(store) = full.surface_store() {
        // Render-target images are freshly allocated and unwritten at
        // registration time — declare UNDEFINED and let the first
        // producer publish their post-release layout via
        // `update_image_layout` once they've issued their QFOT
        // release barrier (#633).
        store.register_texture(
            &handle_id,
            texture,
            produce_done.as_deref(),
            consume_done.as_deref(),
            streamlib_consumer_rhi::VulkanLayout::UNDEFINED,
        )?;
    }
    Ok((handle_id, produce_done, consume_done))
}

/// The `produce_done` / `consume_done` pair for one cross-process
/// registration, or `None` where the device exports no timeline semaphore —
/// the software device refuses an exportable one at creation, and crosses no
/// edge.
fn timeline_edge_pair_where_the_device_exports_one(
    host_device: &crate::vulkan::rhi::HostVulkanDevice,
    caller: &str,
) -> crate::core::error::Result<
    Option<(
        Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
        Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
    )>,
> {
    if !host_device.supports_semaphore_export() {
        return Ok(None);
    }
    Ok(Some((
        new_exportable_timeline_edge(host_device, caller, "produce_done")?,
        new_exportable_timeline_edge(host_device, caller, "consume_done")?,
    )))
}

/// What a device offers a texture that must cross to another process.
#[derive(Clone, Copy, Debug)]
pub(super) struct TextureExportCapabilitiesOfTheDevice {
    /// The device exports DMA-BUF — never so on the software device.
    pub(super) exports_dma_buf: bool,
    /// The render-target probe found an explicit DRM modifier for the
    /// requested format.
    pub(super) has_a_render_target_modifier_for_the_format: bool,
    /// The device built its OPAQUE_FD image pool.
    pub(super) has_the_opaque_fd_image_pool: bool,
}

/// Which cross-process-importable allocation flavor an `acquire_texture`
/// request can take, derived from the request and the device alone — never
/// a Python dial.
///
/// Where the device exports no DMA-BUF, OPAQUE_FD is the one flavor that
/// crosses a process, and every single-plane request takes it whatever its
/// format and usage. Where it does, render-attachment requests need the
/// explicit-modifier DMA-BUF flavor, and OPAQUE_FD is kept to the
/// CUDA-mappable formats within its fixed usage set. Only single-plane
/// formats cross either way — a multi-plane registration ships one fd
/// against N plane offsets, which every consumer import rejects. Everything
/// else keeps the non-importable allocation: a flavor the device or format
/// cannot take falls back rather than failing the acquire, and a later
/// cross-process import refuses by naming the flavor.
pub(super) fn derive_texture_cross_process_importability(
    format: TextureFormat,
    usage: TextureUsages,
    device: TextureExportCapabilitiesOfTheDevice,
) -> TextureCrossProcessImportability {
    let format_is_single_plane = format.plane_count() == 1;
    if !device.exports_dma_buf {
        return if device.has_the_opaque_fd_image_pool && format_is_single_plane {
            TextureCrossProcessImportability::OpaqueFd
        } else {
            TextureCrossProcessImportability::NotImportable
        };
    }
    if usage.contains(TextureUsages::RENDER_ATTACHMENT) {
        return if device.has_a_render_target_modifier_for_the_format && format_is_single_plane {
            TextureCrossProcessImportability::RenderTargetDmaBuf
        } else {
            TextureCrossProcessImportability::NotImportable
        };
    }
    if format.is_cuda_mappable()
        && device.has_the_opaque_fd_image_pool
        && TextureUsages::EVERY_USAGE_A_CUDA_MAPPED_IMAGE_CARRIES.contains(usage)
    {
        return TextureCrossProcessImportability::OpaqueFd;
    }
    TextureCrossProcessImportability::NotImportable
}
