// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use uuid::Uuid;

use super::super::handle_lifecycle::EscalateHandleRegistry;
use super::{new_exportable_timeline_edge, parse_texture_format};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestAcquireImage;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};
use crate::core::context::{
    GpuContextLimitedAccess, PooledTextureHandle, TextureCrossProcessImportability,
    TexturePoolDescriptor,
};
use crate::core::rhi::{TextureFormat, TextureUsages};
use crate::host_rhi::HostSurfaceStoreExt;

/// Acquire one texture from the pool and register it for a helper, answering
/// the handle id the helper names it by.
pub(super) fn acquire_texture_for_helper(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    width: u32,
    height: u32,
    parsed_format: TextureFormat,
    parsed_usage: TextureUsages,
) -> crate::core::error::Result<String> {
    sandbox.escalate(|full| {
        // The importability flavor is derived engine-side from the
        // request — there is no Python dial for it, and a flavor the
        // request cannot take falls back to NotImportable so a later
        // import refuses by name instead of the acquire failing.
        let desc = TexturePoolDescriptor::new(width, height, parsed_format)
            .with_usage(parsed_usage)
            .with_cross_process_importability(match full.host_vulkan_device_arc() {
                Ok(device) => derive_texture_cross_process_importability(
                    parsed_format,
                    parsed_usage,
                    device.has_render_target_modifier_for_texture_format(parsed_format),
                    device.opaque_fd_image_pool().is_some(),
                ),
                Err(_) => TextureCrossProcessImportability::NotImportable,
            });
        let texture = full.acquire_texture(&desc)?;
        let (handle_id, produce_done, consume_done) = assign_texture_handle_id(full, &texture)?;
        // The parent answers its own binding resolutions from the
        // texture cache — without this entry it would re-import its
        // own allocation through the surface-share socket, a path
        // that cannot rebuild every flavour and re-interprets the
        // ones it can.
        full.register_texture(&handle_id, texture.texture_clone());
        registry.insert_texture(handle_id.clone(), texture, produce_done, consume_done);
        Ok(handle_id)
    })
}

/// Acquire a render-target DMA-BUF image on behalf of a helper process,
/// holding it and its timeline pair in `registry` until the helper releases it.
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
    let acquired = sandbox.escalate(|full| {
        let texture = full.acquire_render_target_dma_buf_image(width, height, parsed_format)?;
        let (handle_id, produce_done, consume_done) = assign_image_handle_id(full, &texture)?;
        Ok((handle_id, texture, produce_done, consume_done))
    });
    match acquired {
        Ok((handle_id, texture, produce_done, consume_done)) => {
            // Stash the texture + timeline pair in the
            // registry so the FDs handed to surface-share
            // stay valid for the registration's lifetime.
            registry.insert_image(handle_id.clone(), texture, produce_done, consume_done);
            EscalateResponse::Ok(EscalateResponseOk {
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
            })
        }
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
/// `docs/architecture/adapter-timeline-single-writer.md`). The
/// timelines are returned so the caller can stash them in the
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
        let produce_done =
            new_exportable_timeline_edge(&host_device, "assign_texture_handle_id", "produce_done")?;
        let consume_done =
            new_exportable_timeline_edge(&host_device, "assign_texture_handle_id", "consume_done")?;
        // UNDEFINED at registration: pooled textures sit in the
        // texture pool unowned until the first acquire. The host
        // adapter or escalate-IPC bridge transitions to its
        // workload-specific layout on first use; subsequent
        // releases publish the post-release layout via
        // `update_image_layout`.
        store.register_texture(
            &handle_id,
            texture.texture(),
            Some(produce_done.as_ref()),
            Some(consume_done.as_ref()),
            streamlib_consumer_rhi::VulkanLayout::UNDEFINED,
        )?;
        Ok((handle_id, Some(produce_done), Some(consume_done)))
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
/// Also allocates a single-writer-per-edge timeline pair and registers
/// it with surface-share. Returns the timelines so the caller can
/// stash them in the [`EscalateHandleRegistry`].
pub(super) fn assign_image_handle_id(
    full: &crate::core::context::GpuContextFullAccess,
    texture: &crate::core::rhi::Texture,
) -> crate::core::error::Result<(
    String,
    Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
    Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
)> {
    let handle_id = Uuid::new_v4().to_string();
    let host_device = full.host_vulkan_device_arc()?;
    let produce_done =
        new_exportable_timeline_edge(&host_device, "assign_image_handle_id", "produce_done")?;
    let consume_done =
        new_exportable_timeline_edge(&host_device, "assign_image_handle_id", "consume_done")?;
    if let Some(store) = full.surface_store() {
        // Render-target images are freshly allocated and unwritten at
        // registration time — declare UNDEFINED and let the first
        // producer publish their post-release layout via
        // `update_image_layout` once they've issued their QFOT
        // release barrier (#633).
        store.register_texture(
            &handle_id,
            texture,
            Some(produce_done.as_ref()),
            Some(consume_done.as_ref()),
            streamlib_consumer_rhi::VulkanLayout::UNDEFINED,
        )?;
    }
    Ok((handle_id, produce_done, consume_done))
}

/// Which cross-process-importable allocation flavor an `acquire_texture`
/// request can take, derived from the request alone — never a Python dial.
///
/// Render-attachment requests need the explicit-modifier DMA-BUF flavor (the
/// OPAQUE_FD constructor's fixed usage set has no COLOR_ATTACHMENT), and only
/// single-plane formats take it — a multi-plane registration ships one fd
/// against N plane offsets, which every consumer import rejects. Requests
/// whose format is CUDA-mappable and whose usage fits the fixed set take
/// OPAQUE_FD when the device has the pool for it. Everything else keeps
/// today's non-importable allocation — a flavor the device or format cannot
/// take falls back rather than failing the acquire, and a later
/// cross-process import refuses by naming the flavor.
pub(super) fn derive_texture_cross_process_importability(
    format: TextureFormat,
    usage: TextureUsages,
    render_target_modifier_available: bool,
    opaque_fd_image_pool_available: bool,
) -> TextureCrossProcessImportability {
    if usage.contains(TextureUsages::RENDER_ATTACHMENT) {
        let format_is_single_plane = format.plane_count() == 1;
        return if render_target_modifier_available && format_is_single_plane {
            TextureCrossProcessImportability::RenderTargetDmaBuf
        } else {
            TextureCrossProcessImportability::NotImportable
        };
    }
    let cuda_mappable = matches!(
        format,
        TextureFormat::Rgba8Unorm | TextureFormat::Rgba16Float | TextureFormat::Rgba32Float
    );
    let opaque_fd_fixed_usage_set = TextureUsages::COPY_SRC
        | TextureUsages::COPY_DST
        | TextureUsages::TEXTURE_BINDING
        | TextureUsages::STORAGE_BINDING;
    if cuda_mappable && opaque_fd_image_pool_available && opaque_fd_fixed_usage_set.contains(usage)
    {
        return TextureCrossProcessImportability::OpaqueFd;
    }
    TextureCrossProcessImportability::NotImportable
}
