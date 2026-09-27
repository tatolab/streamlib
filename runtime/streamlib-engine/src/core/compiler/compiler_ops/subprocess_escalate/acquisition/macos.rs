// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use uuid::Uuid;

use super::super::handle_lifecycle::{EscalateHandleRegistry, RegisteredHandle};
use super::TexturePoolWaitWhenExhausted;
use super::new_exportable_timeline_edge;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestAcquireImage;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::EscalateResponseErr;
use crate::core::context::{
    GpuContextFullAccess, GpuContextLimitedAccess, PooledTextureHandle,
    TextureCrossProcessImportability, TexturePoolDescriptor,
};
use crate::core::rhi::{TextureFormat, TextureUsages};

/// A tensor storage buffer that crosses to a helper process is Linux-only
/// until its macOS arm lands.
pub(super) fn allocate_registered_storage_buffer_for_helper(
    _full: &GpuContextFullAccess,
    _tensor_layout: &crate::core::rhi::TensorStorageBufferLayout,
) -> crate::core::error::Result<(String, RegisteredHandle)> {
    Err(crate::core::Error::NotSupported(
        "acquire_storage_buffer is Linux-only until its macOS arm lands (#2404)".to_string(),
    ))
}

/// Allocate one texture and register it for a helper — with the
/// surface-share service and the parent's texture cache — answering the id it
/// is registered under and what holds it alive.
pub(super) fn allocate_registered_texture_for_helper(
    full: &GpuContextFullAccess,
    width: u32,
    height: u32,
    parsed_format: TextureFormat,
    parsed_usage: TextureUsages,
    texture_pool_wait: TexturePoolWaitWhenExhausted,
) -> crate::core::error::Result<(String, RegisteredHandle)> {
    let desc = TexturePoolDescriptor::new(width, height, parsed_format)
        .with_usage(parsed_usage)
        .with_cross_process_importability(match full.host_vulkan_device_arc() {
            Ok(device) => derive_texture_cross_process_importability(
                parsed_format,
                device.supports_metal_objects_interop(),
            ),
            Err(_) => TextureCrossProcessImportability::NotImportable,
        });
    let texture = texture_pool_wait.acquire_texture(full, &desc)?;
    let (handle_id, timeline_pair) = assign_texture_handle_id(full, &texture)?;
    full.register_texture(&handle_id, texture.texture_clone());
    Ok((
        handle_id,
        RegisteredHandle::Texture {
            texture,
            timeline_pair,
        },
    ))
}

/// Resolve the `handle_id` returned to the subprocess for a pooled texture.
///
/// On macOS an IOSurface-backed texture is registered with the surface-share
/// service whole — its surface, image recipe and layout — under a fresh UUID,
/// with a timeline pair minted for it, so a helper can `check_out` it. A
/// texture the device could not allocate over an IOSurface gets an id alone,
/// and nothing a helper can resolve.
pub(super) fn assign_texture_handle_id(
    full: &crate::core::context::GpuContextFullAccess,
    texture: &PooledTextureHandle,
) -> crate::core::error::Result<(
    String,
    Option<Arc<crate::apple::surface_share::CrossProcessTimelinePair>>,
)> {
    let handle_id = Uuid::new_v4().to_string();
    let Some(store) = full.surface_store() else {
        return Ok((handle_id, None));
    };
    if crate::host_rhi::HostTextureExt::vulkan_inner(texture.texture())
        .backing_iosurface()
        .is_none()
    {
        tracing::debug!(
            handle_id,
            "assign_texture_handle_id: the texture is not IOSurface-backed, so no helper can \
             resolve it"
        );
        return Ok((handle_id, None));
    }
    let host_device = full.host_vulkan_device_arc()?;
    let timeline_pair = Arc::new(crate::apple::surface_share::CrossProcessTimelinePair::new(
        new_exportable_timeline_edge(&host_device, "assign_texture_handle_id", "produce_done")?,
        new_exportable_timeline_edge(&host_device, "assign_texture_handle_id", "consume_done")?,
    ));
    store.host_register_texture_with_timeline_pair(
        &handle_id,
        texture.texture(),
        &timeline_pair,
        streamlib_consumer_rhi::VulkanLayout::UNDEFINED,
    )?;
    Ok((handle_id, Some(timeline_pair)))
}

/// The cross-process importability flavor an `acquire_texture` request can
/// take on macOS — an image over a private IOSurface, for any single-plane
/// format, when the device can create one. Everything else keeps the
/// non-importable allocation, and a helper's later resolve refuses.
pub(super) fn derive_texture_cross_process_importability(
    format: TextureFormat,
    device_supports_metal_objects_interop: bool,
) -> TextureCrossProcessImportability {
    if device_supports_metal_objects_interop && format.plane_count() == 1 {
        TextureCrossProcessImportability::IOSurface
    } else {
        TextureCrossProcessImportability::NotImportable
    }
}

pub(in super::super) fn handle_acquire_image(
    _sandbox: &GpuContextLimitedAccess,
    _registry: &EscalateHandleRegistry,
    request_id: String,
    _request: EscalateRequestAcquireImage,
) -> EscalateResponse {
    EscalateResponse::Err(EscalateResponseErr {
        request_id,
        message:
            "acquire_image is not needed on macOS: it hands out a DMA-BUF render-target image, and \
                  on macOS acquire_texture's IOSurface-backed texture is the render target"
                .to_string(),
    })
}
