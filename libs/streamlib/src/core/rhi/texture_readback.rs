// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Public descriptor + error types for the host-side texture-readback RHI primitive.
//!
//! Pattern follows production engines (Unreal `FRHIGPUTextureReadback`, bgfx
//! `bgfx::readTexture`, WebGPU `copyTextureToBuffer + mapAsync`, Granite
//! `copy_image_to_buffer + vkSemaphoreWaitKHR`): caller creates a readback
//! handle bound to a fixed format/extent, then submits copies that return a
//! ticket; the staging buffer + command resources + timeline semaphore are
//! allocated once at construction and reused across submits.

use crate::core::rhi::TextureFormat;

/// Texture-readback descriptor: pin format + extent at construction.
///
/// The handle's staging buffer is sized to `width * height *
/// format.bytes_per_pixel()` and reused across every submit. Submits
/// against a texture whose format/extent disagree with the descriptor
/// return [`TextureReadbackError::DescriptorMismatch`].
#[derive(Debug, Clone)]
pub struct TextureReadbackDescriptor<'a> {
    /// Human-readable label used in error messages and tracing.
    pub label: &'a str,
    /// Pixel format of the texture(s) this readback will copy from.
    pub format: TextureFormat,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Last-known image layout the texture is in when handed to the
/// readback handle's `submit`.
///
/// The readback transitions `source_layout → TRANSFER_SRC_OPTIMAL` for
/// the copy and back to `source_layout` afterward, so the texture is in
/// the same layout before and after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureSourceLayout {
    /// Image was last used in compute / general read-write
    /// (`VK_IMAGE_LAYOUT_GENERAL`). The default for textures produced by
    /// compute kernels and for images coming back from graphics-API
    /// composers (Skia, OpenGL) that don't expose an explicit layout.
    General,
    /// Image was last used as a color attachment in a render pass
    /// (`VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL`).
    ColorAttachment,
    /// Image was last used as a shader-read-only sampled texture
    /// (`VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL`).
    ShaderReadOnly,
}

/// Opaque ticket returned by submit; pass to `try_read` / `wait_and_read`
/// to retrieve the staging buffer's contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadbackTicket {
    /// Identity of the readback handle that issued the ticket. Tickets
    /// from one handle are not valid against another.
    pub(crate) handle_id: u64,
    /// Timeline counter value the handle's signal semaphore reaches when
    /// the GPU copy is complete.
    pub(crate) counter: u64,
}

/// Error taxonomy for the texture-readback RHI primitive.
///
/// Named variants — each carries enough context to diagnose without
/// re-running. `From<TextureReadbackError> for StreamError` (in the
/// host-side impl module) keeps callers in the existing `Result<T>`
/// channel.
#[derive(Debug, thiserror::Error)]
pub enum TextureReadbackError {
    /// Failed to create the staging buffer or its memory.
    #[error("texture readback '{label}': failed to create staging buffer ({size} bytes): {cause}")]
    StagingBufferAlloc {
        label: String,
        size: u64,
        cause: String,
    },
    /// Failed to create the command pool, command buffer, or fence/semaphore.
    #[error("texture readback '{label}': failed to create command resource ({what}): {cause}")]
    CommandResources {
        label: String,
        what: &'static str,
        cause: String,
    },
    /// Texture handed to submit() doesn't match the descriptor's
    /// format/extent.
    #[error(
        "texture readback '{label}': texture (format={actual_format:?}, {actual_width}x{actual_height}) \
         does not match descriptor (format={expected_format:?}, {expected_width}x{expected_height})"
    )]
    DescriptorMismatch {
        label: String,
        expected_format: TextureFormat,
        expected_width: u32,
        expected_height: u32,
        actual_format: TextureFormat,
        actual_width: u32,
        actual_height: u32,
    },
    /// Texture handed to submit() has no Vulkan image handle (e.g. the
    /// texture was constructed for a non-Vulkan backend).
    #[error("texture readback '{label}': texture has no Vulkan image handle")]
    TextureMissingVulkanImage { label: String },
    /// submit() called while a prior submit's ticket hasn't been waited.
    /// The handle is single-in-flight: hold N handles for N parallel
    /// readbacks, mirroring `VulkanComputeKernel`.
    #[error(
        "texture readback '{label}': submit() called with prior ticket (counter={pending}) still in flight"
    )]
    InFlight { label: String, pending: u64 },
    /// try_read / wait_and_read called with no in-flight submission.
    #[error("texture readback '{label}': no in-flight submission to read from")]
    NoSubmission { label: String },
    /// Ticket from a different readback handle was passed.
    #[error(
        "texture readback '{label}': ticket from foreign handle (id {ticket_handle_id}, this handle id {handle_id})"
    )]
    ForeignTicket {
        label: String,
        handle_id: u64,
        ticket_handle_id: u64,
    },
    /// Ticket counter doesn't match the in-flight submission (caller
    /// passed a ticket from a different submit on the same handle —
    /// since the handle is single-in-flight this means a ticket was
    /// reused across submits).
    #[error(
        "texture readback '{label}': ticket counter {ticket} does not match in-flight counter {pending}"
    )]
    StaleTicket {
        label: String,
        ticket: u64,
        pending: u64,
    },
    /// Command recording or queue submission failed.
    #[error("texture readback '{label}': {what} failed: {cause}")]
    Submit {
        label: String,
        what: &'static str,
        cause: String,
    },
    /// Wait timed out (`wait_and_read` with explicit timeout).
    #[error("texture readback '{label}': wait timed out after {timeout_ns}ns")]
    WaitTimeout { label: String, timeout_ns: u64 },
}

impl TextureReadbackDescriptor<'_> {
    /// Total staging-buffer size in bytes for this descriptor.
    pub fn staging_size(&self) -> u64 {
        (self.width as u64) * (self.height as u64) * (self.format.bytes_per_pixel() as u64)
    }
}
