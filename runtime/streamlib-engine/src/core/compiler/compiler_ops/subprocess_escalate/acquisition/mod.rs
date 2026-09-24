// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Pixel buffers, pooled textures and render-target images a helper process
//! acquires, each held until the helper releases it.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod neither_linux_nor_macos;
#[cfg(test)]
mod tests;

use std::sync::Arc;

#[cfg(target_os = "linux")]
use linux::acquire_texture_for_helper;
#[cfg(target_os = "linux")]
pub(super) use linux::handle_acquire_image;
#[cfg(target_os = "macos")]
use macos::acquire_texture_for_helper;
#[cfg(target_os = "macos")]
pub(super) use macos::handle_acquire_image;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use neither_linux_nor_macos::acquire_texture_for_helper;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) use neither_linux_nor_macos::handle_acquire_image;

use super::handle_lifecycle::EscalateHandleRegistry;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestAcquirePixelBuffer, EscalateRequestAcquireTexture,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};
use crate::core::context::GpuContextLimitedAccess;
use crate::core::rhi::{PixelBuffer, PixelFormat, TextureFormat, TextureUsages};

/// Acquire a pixel buffer on behalf of a helper process, holding it in
/// `registry` until the helper releases it.
pub(super) fn handle_acquire_pixel_buffer(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    rid: String,
    request: EscalateRequestAcquirePixelBuffer,
) -> EscalateResponse {
    let EscalateRequestAcquirePixelBuffer {
        request_id: _,
        width,
        height,
        format,
    } = request;
    match PixelFormat::parse_wire_name(&format) {
        Ok(parsed) => {
            let acquired = sandbox.escalate(|full| {
                let (published_frame_id, buffer) =
                    full.acquire_pixel_buffer(width, height, parsed)?;
                let handle_id = assign_buffer_handle_id(full, &published_frame_id, &buffer)?;
                Ok((handle_id, buffer))
            });
            match acquired {
                Ok((handle_id, buffer)) => {
                    registry.insert_buffer(handle_id.clone(), buffer);
                    EscalateResponse::Ok(EscalateResponseOk {
                        request_id: rid,
                        handle_id,
                        width: Some(width),
                        height: Some(height),
                        format: Some(parsed.wire_name().to_string()),
                        ..Default::default()
                    })
                }
                Err(e) => EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: format!("acquire_pixel_buffer failed: {e}"),
                }),
            }
        }
        Err(e) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: e,
        }),
    }
}

/// Acquire a pooled texture on behalf of a helper process, holding it in
/// `registry` until the helper releases it.
pub(super) fn handle_acquire_texture(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    rid: String,
    request: EscalateRequestAcquireTexture,
) -> EscalateResponse {
    let EscalateRequestAcquireTexture {
        request_id: _,
        width,
        height,
        format,
        usage,
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
    let parsed_usage = match parse_texture_usages(&usage) {
        Ok(u) => u,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: e,
            });
        }
    };
    let acquired = acquire_texture_for_helper(
        sandbox,
        registry,
        width,
        height,
        parsed_format,
        parsed_usage,
    );
    match acquired {
        Ok(handle_id) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            handle_id,
            width: Some(width),
            height: Some(height),
            format: Some(parsed_format.wire_name().to_string()),
            usage: Some(texture_usages_to_wire(parsed_usage)),
            ..Default::default()
        }),
        Err(e) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("acquire_texture failed: {e}"),
        }),
    }
}

/// Resolve the `handle_id` returned to the subprocess for a pixel buffer.
///
/// On Linux, the buffer is checked in with the surface-share service so the polyglot
/// subprocess shim can later `check_out` the DMA-BUF FD; the surface-share service-assigned
/// `surface_id` becomes the handle_id. On other platforms the published frame
/// id stays as-is: a macOS pool slot is already registered with the
/// surface-share service under its slot key, so the frame id resolves there.
#[allow(unused_variables)]
pub(super) fn assign_buffer_handle_id(
    full: &crate::core::context::GpuContextFullAccess,
    published_frame_id: &crate::core::rhi::PublishedPixelBufferFrameId,
    buffer: &PixelBuffer,
) -> crate::core::error::Result<String> {
    #[cfg(target_os = "linux")]
    {
        if let Some(store) = full.surface_store() {
            return store.check_in(buffer);
        }
    }
    Ok(published_frame_id.to_string())
}

/// A fresh exportable timeline for one edge of a cross-process pair, on the
/// host device; `caller` and `edge` name it in a refusal.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn new_exportable_timeline_edge(
    host_device: &crate::vulkan::rhi::HostVulkanDevice,
    caller: &str,
    edge: &str,
) -> crate::core::error::Result<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>> {
    crate::vulkan::rhi::HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0)
        .map(Arc::new)
        .map_err(|e| {
            crate::core::error::Error::GpuError(format!("{caller}: new_exportable ({edge}): {e}"))
        })
}

/// Parse a wire-format texture format string into a [`TextureFormat`].
///
/// Lowercase snake-case matches the variant name. A separate vocabulary
/// from [`PixelFormat::parse_wire_name`] — pixel formats include
/// video-specific YUV variants that textures don't expose, and texture
/// formats include float variants that pixel buffers don't.
pub(super) fn parse_texture_format(s: &str) -> std::result::Result<TextureFormat, String> {
    let normalized = s.trim().to_ascii_lowercase();
    TextureFormat::from_wire_name(&normalized)
        .ok_or_else(|| format!("unknown texture format '{normalized}'"))
}

/// Parse an array of usage tokens into a combined [`TextureUsages`] bitmask,
/// with both copy bits implied.
///
/// An empty list is rejected — a texture must have at least one usage or the
/// RHI can't create it. Unknown tokens surface as an error so typos fail
/// loudly on the wire rather than silently dropping flags.
///
/// `COPY_SRC | COPY_DST` ride every request because the CPU doors copy both
/// ways over a texture's export staging, so an author who acquired a texture
/// to fill it would otherwise be refused about a transfer flag rather than a
/// real constraint.
pub(super) fn parse_texture_usages(
    tokens: &[String],
) -> std::result::Result<TextureUsages, String> {
    if tokens.is_empty() {
        return Err("texture usage list must not be empty".to_string());
    }
    let mut out = TextureUsages::COPY_SRC | TextureUsages::COPY_DST;
    for token in tokens {
        let normalized = token.trim().to_ascii_lowercase();
        let flag = match normalized.as_str() {
            "copy_src" => TextureUsages::COPY_SRC,
            "copy_dst" => TextureUsages::COPY_DST,
            "texture_binding" => TextureUsages::TEXTURE_BINDING,
            "storage_binding" => TextureUsages::STORAGE_BINDING,
            "render_attachment" => TextureUsages::RENDER_ATTACHMENT,
            other => return Err(format!("unknown texture usage '{other}'")),
        };
        out |= flag;
    }
    Ok(out)
}

pub(super) fn texture_usages_to_wire(usage: TextureUsages) -> Vec<String> {
    let mut out = Vec::new();
    if usage.contains(TextureUsages::COPY_SRC) {
        out.push("copy_src".to_string());
    }
    if usage.contains(TextureUsages::COPY_DST) {
        out.push("copy_dst".to_string());
    }
    if usage.contains(TextureUsages::TEXTURE_BINDING) {
        out.push("texture_binding".to_string());
    }
    if usage.contains(TextureUsages::STORAGE_BINDING) {
        out.push("storage_binding".to_string());
    }
    if usage.contains(TextureUsages::RENDER_ATTACHMENT) {
        out.push("render_attachment".to_string());
    }
    out
}
