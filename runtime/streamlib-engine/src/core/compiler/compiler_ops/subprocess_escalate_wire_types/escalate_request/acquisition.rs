// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestAcquireImage {
    /// Texture format identifier. Lowercase snake-case names: bgra8_unorm,
    /// bgra8_unorm_srgb, rgba8_unorm, rgba8_unorm_srgb. The host
    /// backs this with a render-target-capable VkImage allocated via
    /// VK_EXT_image_drm_format_modifier and a tiled DRM modifier picked
    /// from the EGL `external_only=FALSE` list — the resulting DMA-BUF can
    /// be imported by the consumer as a GL_TEXTURE_2D color attachment.
    /// Returns an error when the EGL probe didn't find an RT-capable modifier
    /// for `format` (no fallback to LINEAR — sampler-only on NVIDIA, see
    /// NVIDIA EGL's sampler-only linear DMA-BUFs).
    /// Internal host primitive — surface adapters (streamlib-adapter-vulkan /
    /// -opengl / -skia) use this on customers' behalf; customers never invoke
    /// acquire_image directly.
    pub(crate) format: String,

    /// Pixel height of the image.
    pub(crate) height: u32,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// Pixel width of the image.
    pub(crate) width: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestAcquirePixelBuffer {
    /// Pixel format identifier (e.g. bgra32, nv12_video_range, gray8).
    pub(crate) format: String,

    /// Pixel height of the buffer.
    pub(crate) height: u32,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// Pixel width of the buffer.
    pub(crate) width: u32,
}

/// Acquire a tensor storage buffer: engine-owned memory of a declared shape and
/// element type, named by surface id and importable by CUDA.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestAcquireStorageBuffer {
    /// Element type: float32, float16, uint8 or int32.
    pub(crate) dtype: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) node_output_pool: Option<EscalateRequestNodeOutputPool>,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// Dimensions, outermost first; contiguous row-major, every one non-zero.
    pub(crate) shape: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestAcquireTexture {
    /// Texture format identifier. Lowercase snake-case names: rgba8_unorm,
    /// rgba8_unorm_srgb, bgra8_unorm, bgra8_unorm_srgb, rgba16_float,
    /// rgba32_float, nv12.
    pub(crate) format: String,

    /// Pixel height of the texture.
    pub(crate) height: u32,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) node_output_pool: Option<EscalateRequestNodeOutputPool>,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// Usage flags the texture must support. Non-empty array of lowercase
    /// snake-case tokens drawn from: copy_src, copy_dst, texture_binding,
    /// storage_binding, render_attachment. Host validates — unknown tokens
    /// return an error response.
    pub(crate) usage: Vec<String>,

    /// Pixel width of the texture.
    pub(crate) width: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestNodeOutputPool {
    /// The pool's name, minted by the helper's ring; acquires under one key
    /// share one pool, scoped to the helper that sent them.
    pub(crate) pool_key: String,

    /// How many slots the pool rotates through before reusing one nobody
    /// holds — how many publishes an unclaimed frame stays resolvable for.
    pub(crate) rotation_depth: u32,
}
