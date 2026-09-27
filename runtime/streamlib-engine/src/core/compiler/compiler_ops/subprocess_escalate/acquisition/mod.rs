// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Pixel buffers, pooled textures, tensor storage buffers and render-target
//! images a helper process acquires, each held until the helper releases it.

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
pub(super) use linux::handle_acquire_image;
#[cfg(target_os = "linux")]
use linux::{
    allocate_registered_storage_buffer_for_helper, allocate_registered_texture_for_helper,
};
#[cfg(target_os = "macos")]
pub(super) use macos::handle_acquire_image;
#[cfg(target_os = "macos")]
use macos::{
    allocate_registered_storage_buffer_for_helper, allocate_registered_texture_for_helper,
};
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) use neither_linux_nor_macos::handle_acquire_image;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use neither_linux_nor_macos::{
    allocate_registered_storage_buffer_for_helper, allocate_registered_texture_for_helper,
};

use super::handle_lifecycle::{
    EscalateHandleRegistry, ProcessorOutputFrameHandOff, ProcessorOutputFreshSlotHandOff,
    ProcessorOutputSlotDescriptor, ProcessorOutputTextureDescriptor, RegisteredHandle,
    release_processor_output_pool_slot,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestAcquirePixelBuffer, EscalateRequestAcquireStorageBuffer,
    EscalateRequestAcquireTexture, EscalateRequestProcessorOutputPool,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};
use crate::core::context::GpuContextLimitedAccess;
use crate::core::rhi::{
    PixelBuffer, PixelFormat, TensorStorageBufferLayout, TextureFormat, TextureUsages,
};

/// Whether a texture allocation may wait on the texture pool's exhaustion
/// policy for another holder to let a texture go.
#[derive(Clone, Copy)]
pub(super) enum TexturePoolWaitWhenExhausted {
    /// Whatever the pool is configured to do — a one-off acquire.
    AsThePoolIsConfigured,
    /// Refuse at once: a processor output pool's producer never waits.
    RefuseAtOnce,
}

impl TexturePoolWaitWhenExhausted {
    fn acquire_texture(
        self,
        full: &crate::core::context::GpuContextFullAccess,
        desc: &crate::core::context::TexturePoolDescriptor,
    ) -> crate::core::error::Result<crate::core::context::PooledTextureHandle> {
        match self {
            Self::AsThePoolIsConfigured => full.acquire_texture(desc),
            Self::RefuseAtOnce => full.acquire_texture_without_waiting(desc),
        }
    }
}

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

/// Acquire a texture on behalf of a helper process: a one-off the registry
/// holds until the helper releases it, or the next frame of a processor output
/// pool, which the pool holds.
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
        processor_output_pool,
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
    let acquired = match processor_output_pool {
        None => sandbox.escalate(|full| {
            let (handle_id, registered_texture) = allocate_registered_texture_for_helper(
                full,
                width,
                height,
                parsed_format,
                parsed_usage,
                TexturePoolWaitWhenExhausted::AsThePoolIsConfigured,
            )?;
            registry.insert_registered_handle(handle_id.clone(), registered_texture);
            Ok(handle_id)
        }),
        Some(processor_output_pool) => hand_off_processor_output_frame(
            sandbox,
            registry,
            processor_output_pool,
            &ProcessorOutputSlotDescriptor::Texture(ProcessorOutputTextureDescriptor {
                width,
                height,
                format: parsed_format,
                usage: parsed_usage,
            }),
        ),
    };
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

/// Acquire a tensor storage buffer on behalf of a helper process: a one-off
/// the registry holds until the helper releases it, or the next frame of a
/// processor output pool, which the pool holds.
pub(super) fn handle_acquire_storage_buffer(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    rid: String,
    request: EscalateRequestAcquireStorageBuffer,
) -> EscalateResponse {
    let EscalateRequestAcquireStorageBuffer {
        request_id: _,
        shape,
        dtype,
        processor_output_pool,
    } = request;
    let tensor_layout = match TensorStorageBufferLayout::from_wire(shape, &dtype) {
        Ok(tensor_layout) => tensor_layout,
        Err(refusal) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("acquire_storage_buffer refused: {refusal}"),
            });
        }
    };
    let acquired = match processor_output_pool {
        None => sandbox.escalate(|full| {
            let (handle_id, registered_buffer) =
                allocate_registered_storage_buffer_for_helper(full, &tensor_layout)?;
            registry.insert_registered_handle(handle_id.clone(), registered_buffer);
            Ok(handle_id)
        }),
        Some(processor_output_pool) => hand_off_processor_output_frame(
            sandbox,
            registry,
            processor_output_pool,
            &ProcessorOutputSlotDescriptor::StorageBuffer(tensor_layout.clone()),
        ),
    };
    match acquired {
        Ok(handle_id) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            handle_id,
            shape: Some(tensor_layout.shape().to_vec()),
            dtype: Some(tensor_layout.element_type().wire_name().to_string()),
            ..Default::default()
        }),
        Err(e) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("acquire_storage_buffer failed: {e}"),
        }),
    }
}

/// Allocate and register one fresh slot of the kind `descriptor` names,
/// refusing at once rather than waiting on an exhausted allocator.
fn allocate_registered_processor_output_slot(
    full: &crate::core::context::GpuContextFullAccess,
    descriptor: &ProcessorOutputSlotDescriptor,
) -> crate::core::error::Result<(String, RegisteredHandle)> {
    match descriptor {
        ProcessorOutputSlotDescriptor::Texture(texture_descriptor) => {
            allocate_registered_texture_for_helper(
                full,
                texture_descriptor.width,
                texture_descriptor.height,
                texture_descriptor.format,
                texture_descriptor.usage,
                TexturePoolWaitWhenExhausted::RefuseAtOnce,
            )
        }
        ProcessorOutputSlotDescriptor::StorageBuffer(tensor_layout) => {
            allocate_registered_storage_buffer_for_helper(full, tensor_layout)
        }
    }
}

/// Hand out the next frame of one of the helper's processor output pools,
/// answering the frame's published id.
///
/// Only a fresh slot's allocation enters the escalate scope. The pool lock is
/// never held across it: the helper's teardown drains the pools under that
/// lock, and can run while another scope holds the gate.
fn hand_off_processor_output_frame(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    EscalateRequestProcessorOutputPool {
        pool_key,
        rotation_depth,
    }: EscalateRequestProcessorOutputPool,
    descriptor: &ProcessorOutputSlotDescriptor,
) -> crate::core::error::Result<String> {
    let host = sandbox.host_inner();
    let (handed_off, slots_a_descriptor_change_released) = registry
        .processor_output_pools()
        .hand_off_a_reusable_frame(host, &pool_key, rotation_depth as usize, descriptor);
    for released_slot in slots_a_descriptor_change_released {
        release_processor_output_pool_slot(sandbox, released_slot);
    }
    if let ProcessorOutputFrameHandOff::Published(published_frame_id) = handed_off? {
        return Ok(published_frame_id);
    }
    let (pool_slot_key, registered_handle) =
        sandbox.escalate(|full| allocate_registered_processor_output_slot(full, descriptor))?;
    let fresh_slot_handed_off = registry.processor_output_pools().hand_off_a_fresh_slot(
        host,
        &pool_key,
        descriptor,
        pool_slot_key,
        registered_handle,
    );
    match fresh_slot_handed_off {
        ProcessorOutputFreshSlotHandOff::Published(published_frame_id) => Ok(published_frame_id),
        ProcessorOutputFreshSlotHandOff::Refused {
            refusal,
            slot_owed_its_release,
        } => {
            release_processor_output_pool_slot(sandbox, slot_owed_its_release);
            Err(refusal)
        }
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
