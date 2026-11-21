// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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
use linux::allocate_registered_texture_for_helper;
#[cfg(target_os = "linux")]
pub(super) use linux::handle_acquire_image;
#[cfg(target_os = "macos")]
use macos::allocate_registered_texture_for_helper;
#[cfg(target_os = "macos")]
pub(super) use macos::handle_acquire_image;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) use neither_linux_nor_macos::handle_acquire_image;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use neither_linux_nor_macos::{
    allocate_registered_storage_buffer_for_helper, allocate_registered_texture_for_helper,
};

use super::handle_lifecycle::{
    EscalateHandleRegistry, NodeOutputFrameHandOff, NodeOutputFramePublished,
    NodeOutputFreshSlotHandOff, NodeOutputSlotDescriptor, NodeOutputTextureDescriptor,
    RegisteredHandle, release_node_output_pool_slot,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestAcquirePixelBuffer, EscalateRequestAcquireStorageBuffer,
    EscalateRequestAcquireTexture, EscalateRequestNodeOutputPool,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseGpuPoolSlotFrame, EscalateResponseOk,
};
use crate::core::context::{GpuContextLimitedAccess, OneOffAcquisitionShape};
use crate::core::rhi::{
    PixelBuffer, PixelFormat, TensorStorageBufferLayout, TextureFormat, TextureUsages,
    texture_usages_to_wire,
};

/// Acquire a pixel buffer on behalf of a helper process, holding it in
/// `registry` until the helper releases it; counted against the node's
/// one-off acquisitions of its shape.
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
            let acquired = registry.acquire_a_one_off_for_the_helper_counted_under_its_shape(
                sandbox,
                OneOffAcquisitionShape::PixelBuffer {
                    width,
                    height,
                    format: parsed,
                },
                |full| {
                    let (published_frame_id, buffer) = full
                        .host_inner()
                        .acquire_pixel_buffer(width, height, parsed)?;
                    let handle_id = assign_buffer_handle_id(full, &published_frame_id, &buffer)?;
                    Ok((handle_id, RegisteredHandle::PixelBuffer(buffer)))
                },
            );
            match acquired {
                Ok(handle_id) => EscalateResponse::Ok(EscalateResponseOk {
                    request_id: rid,
                    handle_id,
                    width: Some(width),
                    height: Some(height),
                    format: Some(parsed.wire_name().to_string()),
                    ..Default::default()
                }),
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
        node_output_pool,
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
    let acquired = match node_output_pool {
        None => registry
            .acquire_a_one_off_for_the_helper_counted_under_its_shape(
                sandbox,
                OneOffAcquisitionShape::Texture {
                    width,
                    height,
                    format: parsed_format,
                    usage: parsed_usage,
                },
                |full| {
                    allocate_registered_texture_for_helper(
                        full,
                        width,
                        height,
                        parsed_format,
                        parsed_usage,
                    )
                },
            )
            .map(NodeOutputFramePublished::of_a_one_off),
        Some(node_output_pool) => hand_off_node_output_frame(
            sandbox,
            registry,
            node_output_pool,
            &NodeOutputSlotDescriptor::Texture(NodeOutputTextureDescriptor {
                width,
                height,
                format: parsed_format,
                usage: parsed_usage,
            }),
        ),
    };
    match acquired {
        Ok(NodeOutputFramePublished {
            published_frame_id: handle_id,
            gpu_pool_slot_frame,
        }) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            handle_id,
            gpu_pool_slot_frame: gpu_pool_slot_frame
                .map(EscalateResponseGpuPoolSlotFrame::of_the_body),
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
        node_output_pool,
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
    let acquired = match node_output_pool {
        None => registry
            .acquire_a_one_off_for_the_helper_counted_under_its_shape(
                sandbox,
                OneOffAcquisitionShape::StorageBuffer {
                    tensor_layout: tensor_layout.clone(),
                },
                |full| allocate_registered_storage_buffer_for_helper(full, &tensor_layout),
            )
            .map(NodeOutputFramePublished::of_a_one_off),
        Some(node_output_pool) => hand_off_node_output_frame(
            sandbox,
            registry,
            node_output_pool,
            &NodeOutputSlotDescriptor::StorageBuffer(tensor_layout.clone()),
        ),
    };
    match acquired {
        Ok(NodeOutputFramePublished {
            published_frame_id: handle_id,
            gpu_pool_slot_frame,
        }) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            handle_id,
            gpu_pool_slot_frame: gpu_pool_slot_frame
                .map(EscalateResponseGpuPoolSlotFrame::of_the_body),
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

/// Allocate one tensor storage buffer that crosses to a helper and register it
/// — with the surface-share service as a `storage_buffer` surface and in the
/// parent-wide storage buffer map — answering the id it is registered under
/// and what holds it alive.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn allocate_registered_storage_buffer_for_helper(
    full: &crate::core::context::GpuContextFullAccess,
    tensor_layout: &TensorStorageBufferLayout,
) -> crate::core::error::Result<(String, RegisteredHandle)> {
    use crate::host_rhi::HostSurfaceStoreExt as _;

    let host = full.host_inner();
    let buffer = host.acquire_storage_buffer_of_flavour(
        tensor_layout,
        crate::core::context::StorageBufferAllocationFlavour::CrossesToAHelperProcess,
    )?;
    let handle_id = uuid::Uuid::new_v4().to_string();
    if let Some(store) = full.surface_store() {
        store.register_storage_buffer(&handle_id, &buffer, tensor_layout)?;
    }
    host.register_storage_buffer_in_the_parent_wide_map(&handle_id, buffer.clone());
    Ok((handle_id, RegisteredHandle::StorageBuffer { buffer }))
}

/// Allocate and register one fresh slot of the kind `descriptor` names.
fn allocate_registered_node_output_slot(
    full: &crate::core::context::GpuContextFullAccess,
    descriptor: &NodeOutputSlotDescriptor,
) -> crate::core::error::Result<(String, RegisteredHandle)> {
    match descriptor {
        NodeOutputSlotDescriptor::Texture(texture_descriptor) => {
            allocate_registered_texture_for_helper(
                full,
                texture_descriptor.width,
                texture_descriptor.height,
                texture_descriptor.format,
                texture_descriptor.usage,
            )
        }
        NodeOutputSlotDescriptor::StorageBuffer(tensor_layout) => {
            allocate_registered_storage_buffer_for_helper(full, tensor_layout)
        }
    }
}

fn hand_off_node_output_frame(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    EscalateRequestNodeOutputPool {
        pool_key,
        rotation_depth,
    }: EscalateRequestNodeOutputPool,
    descriptor: &NodeOutputSlotDescriptor,
) -> crate::core::error::Result<NodeOutputFramePublished> {
    let host = sandbox.host_inner();
    host.slots_outliving_their_producers()
        .release_each_slot_nobody_holds(sandbox);
    let (handed_off, slots_a_descriptor_change_released) = registry
        .node_output_pools()
        .hand_off_a_reusable_frame(host, &pool_key, rotation_depth as usize, descriptor);
    for released_slot in slots_a_descriptor_change_released {
        release_node_output_pool_slot(sandbox, released_slot);
    }
    if let NodeOutputFrameHandOff::Published(published) = handed_off? {
        return Ok(published);
    }
    let (pool_slot_key, registered_handle) =
        sandbox.escalate(|full| allocate_registered_node_output_slot(full, descriptor))?;
    let fresh_slot_handed_off = registry.node_output_pools().hand_off_a_fresh_slot(
        host,
        &pool_key,
        descriptor,
        pool_slot_key,
        registered_handle,
    );
    match fresh_slot_handed_off {
        NodeOutputFreshSlotHandOff::Published(published) => Ok(published),
        NodeOutputFreshSlotHandOff::Refused {
            refusal,
            slot_owed_its_release,
        } => {
            release_node_output_pool_slot(sandbox, slot_owed_its_release);
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
#[allow(
    unused_variables,
    reason = "only the Linux build reads the context and the buffer"
)]
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
