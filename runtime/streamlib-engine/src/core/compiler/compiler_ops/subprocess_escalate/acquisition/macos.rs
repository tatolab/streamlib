// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use uuid::Uuid;

use super::super::handle_lifecycle::EscalateHandleRegistry;
use super::new_exportable_timeline_edge;
use crate::core::context::{
    GpuContextLimitedAccess, PooledTextureHandle, TextureCrossProcessImportability,
    TexturePoolDescriptor,
};
use crate::core::rhi::{TextureFormat, TextureUsages};

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
        let desc = TexturePoolDescriptor::new(width, height, parsed_format)
            .with_usage(parsed_usage)
            .with_cross_process_importability(match full.host_vulkan_device_arc() {
                Ok(device) => derive_texture_cross_process_importability(
                    parsed_format,
                    device.supports_metal_objects_interop(),
                ),
                Err(_) => TextureCrossProcessImportability::NotImportable,
            });
        let texture = full.acquire_texture(&desc)?;
        let (handle_id, timeline_pair) = assign_texture_handle_id(full, &texture)?;
        full.register_texture(&handle_id, texture.texture_clone());
        registry.insert_texture(handle_id.clone(), texture, timeline_pair);
        Ok(handle_id)
    })
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
