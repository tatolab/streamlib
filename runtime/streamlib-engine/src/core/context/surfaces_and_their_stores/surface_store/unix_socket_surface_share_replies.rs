// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

use crate::core::context::surfaces_and_their_stores::surface_share_wire_verbs::stated_vk_memory_type_index;
use crate::core::rhi::PixelBuffer;
use crate::core::{Error, Result};

use super::SURFACE_HANDLE_TYPE_DMA_BUF;
use super::registration_payloads::{
    SURFACE_REPLY_HAS_CONSUME_DONE_FD, SURFACE_REPLY_HAS_PRODUCE_DONE_FD,
    external_plane_handles_for_flavour,
};

/// Adopt the fds a surface-share reply delivered over SCM_RIGHTS.
///
/// Each one landed in this process's table as a fresh descriptor that no
/// other party closes, so adopting them the moment they arrive is what
/// makes every exit of a lookup — a wire error, a refusal, a failed
/// import — close them exactly once.
pub(super) fn adopt_reply_fds(received_fds: Vec<std::os::fd::RawFd>) -> Vec<OwnedFd> {
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
pub(super) fn plane_fds_of_reply(
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

/// Where a single-plane texture with a tiled DRM format modifier sits in its
/// DMA-BUF, and the usage its registration states, as a lookup reply carries
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TiledDmaBufTextureOfALookupReply {
    pub(super) plane_offset_bytes: u64,
    pub(super) plane_stride_bytes: u64,
    pub(super) allocation_byte_size: u64,
    pub(super) usage: streamlib_consumer_rhi::VulkanImageUsage,
}

impl TiledDmaBufTextureOfALookupReply {
    /// The one plane's offset and stride, the allocation's size — the larger
    /// of `plane_sizes[0]` and `vk_image_allocation_size` — and the usage,
    /// refused unless the reply states exactly one offset and one stride.
    pub(super) fn of(response: &serde_json::Value) -> Result<Self> {
        let only_plane_value = |key: &str| -> Result<u64> {
            match response
                .get(key)
                .and_then(serde_json::Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .map(serde_json::Value::as_u64)
                        .collect::<Vec<_>>()
                })
                .as_deref()
            {
                Some([Some(value)]) => Ok(*value),
                _ => Err(Error::Configuration(format!(
                    "lookup_texture: a texture with a tiled DRM format modifier states one \
                     plane's {key}, and the reply states {:?}",
                    response.get(key)
                ))),
            }
        };
        let plane_size_bytes = plane_sizes_of_reply(response, 1)[0];
        let vk_image =
            crate::core::context::surfaces_and_their_stores::surface_share_wire_verbs::parse_vk_image_create_info_fields(
                response,
            );
        Ok(Self {
            plane_offset_bytes: only_plane_value("plane_offsets")?,
            plane_stride_bytes: only_plane_value("plane_strides")?,
            allocation_byte_size: plane_size_bytes.max(vk_image.vk_image_allocation_size),
            usage: streamlib_consumer_rhi::VulkanImageUsage(vk_image.vk_image_usage),
        })
    }
}

/// The `plane_sizes` a lookup reply states, one per plane fd; zeros when
/// the reply states none or a different count.
pub(super) fn plane_sizes_of_reply(response: &serde_json::Value, plane_count: usize) -> Vec<u64> {
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
fn leave_plane_fds_to_the_import(plane_fds: Vec<OwnedFd>) {
    use std::os::fd::IntoRawFd as _;
    for plane_fd in plane_fds {
        let _ = plane_fd.into_raw_fd();
    }
}

/// Import the pixel buffer a surface-share `operation` reply carries, under
/// the external-handle flavour the service says it registered; a reply
/// naming an error is refused under `operation`'s name.
pub(super) fn pixel_buffer_imported_from_surface_share_reply(
    operation: &str,
    response: &serde_json::Value,
    reply_fds: Vec<OwnedFd>,
) -> Result<PixelBuffer> {
    use crate::core::rhi::{PixelFormat, RhiPixelBufferImport};

    if let Some(error) = response.get("error").and_then(serde_json::Value::as_str) {
        return Err(Error::Configuration(format!("{operation}: {error}")));
    }
    let plane_fds = plane_fds_of_reply(operation, response, reply_fds)?;

    // The two flavours are different Vulkan external-handle types, so
    // importing an OPAQUE_FD fd as DMA-BUF is a driver-level wrong-import
    // rather than a clean failure.
    let plane_sizes = plane_sizes_of_reply(response, plane_fds.len());
    let raw_plane_fds: Vec<std::os::fd::RawFd> = plane_fds.iter().map(OwnedFd::as_raw_fd).collect();
    let handles = external_plane_handles_for_flavour(
        operation,
        response
            .get("handle_type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(SURFACE_HANDLE_TYPE_DMA_BUF),
        &raw_plane_fds,
        &plane_sizes,
        stated_vk_memory_type_index(response),
    )?;
    leave_plane_fds_to_the_import(plane_fds);
    PixelBuffer::from_external_plane_handles(handles, 0, 0, PixelFormat::default())
}

/// The format, extent and plane layout a `lookup` reply states for an
/// allocation of `allocation_byte_size` bytes — a tight layout when it states
/// none.
pub(super) fn pixel_format_extent_and_planes_of_lookup_reply(
    surface_id: &str,
    response: &serde_json::Value,
    allocation_byte_size: u64,
) -> Result<(
    crate::core::rhi::PixelFormat,
    u32,
    u32,
    crate::core::rhi::PixelPlanesWithinOneAllocation,
)> {
    use crate::core::rhi::{PixelFormat, PixelPlanesWithinOneAllocation};
    let stated_u32 = |key: &str| {
        response
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0)
    };
    let width = stated_u32("width");
    let height = stated_u32("height");
    let pixel_format = PixelFormat::parse_wire_name(
        response
            .get("format")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown"),
    )
    .map_err(|unknown_format| {
        Error::Configuration(format!("lookup of '{surface_id}': {unknown_format}"))
    })?;
    let stated = match PixelPlanesWithinOneAllocation::from_surface_share_fields(response)? {
        Some(stated) => stated,
        None => PixelPlanesWithinOneAllocation::tightly_packed(pixel_format, width, height)?,
    };
    let planes = PixelPlanesWithinOneAllocation::new(
        pixel_format,
        width,
        height,
        allocation_byte_size,
        stated.plane_offsets_bytes().to_vec(),
        stated.plane_strides_bytes().to_vec(),
        stated.written_by_a_foreign_device(),
    )?;
    Ok((pixel_format, width, height, planes))
}
