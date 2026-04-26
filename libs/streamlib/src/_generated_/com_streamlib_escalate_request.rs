// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.


use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Polyglot subprocess escalate-on-behalf request (subprocess → host)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum EscalateRequest {
    #[serde(rename = "acquire_image")]
    AcquireImage(EscalateRequestAcquireImage),

    #[serde(rename = "acquire_pixel_buffer")]
    AcquirePixelBuffer(EscalateRequestAcquirePixelBuffer),

    #[serde(rename = "acquire_texture")]
    AcquireTexture(EscalateRequestAcquireTexture),

    #[serde(rename = "log")]
    Log(EscalateRequestLog),

    #[serde(rename = "release_handle")]
    ReleaseHandle(EscalateRequestReleaseHandle),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestAcquireImage {
    /// Texture format identifier. Lowercase snake-case names: bgra8_unorm,
    /// bgra8_unorm_srgb, rgba8_unorm, rgba8_unorm_srgb. The host
    /// backs this with a render-target-capable VkImage allocated via
    /// VK_EXT_image_drm_format_modifier and a tiled DRM modifier picked
    /// from the EGL `external_only=FALSE` list — the resulting DMA-BUF can
    /// be imported by the consumer as a GL_TEXTURE_2D color attachment.
    /// Returns an error when the EGL probe didn't find an RT-capable modifier
    /// for `format` (no fallback to LINEAR — sampler-only on NVIDIA, see
    /// docs/learnings/nvidia-egl-dmabuf-render-target.md).
    /// Internal host primitive — surface adapters (streamlib-adapter-vulkan /
    /// -opengl / -skia) use this on customers' behalf; customers never invoke
    /// acquire_image directly.
    #[serde(rename = "format")]
    pub format: String,

    /// Pixel height of the image.
    #[serde(rename = "height")]
    pub height: u32,

    /// Correlates request with response. UUID string.
    #[serde(rename = "request_id")]
    pub request_id: String,

    /// Pixel width of the image.
    #[serde(rename = "width")]
    pub width: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestAcquirePixelBuffer {
    /// Pixel format identifier (e.g. bgra32, nv12_video_range, gray8).
    #[serde(rename = "format")]
    pub format: String,

    /// Pixel height of the buffer.
    #[serde(rename = "height")]
    pub height: u32,

    /// Correlates request with response. UUID string.
    #[serde(rename = "request_id")]
    pub request_id: String,

    /// Pixel width of the buffer.
    #[serde(rename = "width")]
    pub width: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestAcquireTexture {
    /// Texture format identifier. Lowercase snake-case names: rgba8_unorm,
    /// rgba8_unorm_srgb, bgra8_unorm, bgra8_unorm_srgb, rgba16_float,
    /// rgba32_float, nv12.
    #[serde(rename = "format")]
    pub format: String,

    /// Pixel height of the texture.
    #[serde(rename = "height")]
    pub height: u32,

    /// Correlates request with response. UUID string.
    #[serde(rename = "request_id")]
    pub request_id: String,

    /// Usage flags the texture must support. Non-empty array of lowercase
    /// snake-case tokens drawn from: copy_src, copy_dst, texture_binding,
    /// storage_binding, render_attachment. Host validates — unknown tokens
    /// return an error response.
    #[serde(rename = "usage")]
    pub usage: Vec<String>,

    /// Pixel width of the texture.
    #[serde(rename = "width")]
    pub width: u32,
}

/// Severity level of the record. Maps 1:1 onto tracing::Level.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestLogLevel {
    #[serde(rename = "debug")]
    #[default]
    Debug,

    #[serde(rename = "error")]
    Error,

    #[serde(rename = "info")]
    Info,

    #[serde(rename = "trace")]
    Trace,

    #[serde(rename = "warn")]
    Warn,
}

/// Origin runtime of the record. Always "python" or "deno" on the wire — Rust
/// never routes through escalate; Rust call sites hit `tracing::*!()` directly
/// on the host.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum EscalateRequestLogSource {
    #[serde(rename = "deno")]
    #[default]
    Deno,

    #[serde(rename = "python")]
    Python,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestLog {
    /// User-supplied structured fields. Copied flat onto the emitted
    /// RuntimeLogEvent's `attrs` map — not nested under an `attrs.key` path in
    /// the JSONL.
    #[serde(rename = "attrs")]
    pub attrs: HashMap<String, Option<Value>>,

    /// Interceptor channel when `intercepted: true`. Conventional values:
    /// "stdout", "stderr", "console.log", "logging", "fd1", "fd2". Null when
    /// `intercepted: false`.
    #[serde(rename = "channel")]
    pub channel: Option<String>,

    /// True when the record was captured from subprocess stdout/stderr,
    /// console.log, root logging handler, or a raw fd write, rather than a
    /// direct `streamlib.log.*` call.
    #[serde(rename = "intercepted")]
    pub intercepted: bool,

    /// Severity level of the record. Maps 1:1 onto tracing::Level.
    #[serde(rename = "level")]
    pub level: EscalateRequestLogLevel,

    /// Primary human-readable message.
    #[serde(rename = "message")]
    pub message: String,

    /// Pipeline identifier. Null for runtime-level records.
    #[serde(rename = "pipeline_id")]
    pub pipeline_id: Option<String>,

    /// Processor identifier. Null outside a processor.
    #[serde(rename = "processor_id")]
    pub processor_id: Option<String>,

    /// Origin runtime of the record. Always "python" or "deno" on the wire —
    /// Rust never routes through escalate; Rust call sites hit `tracing::*!()`
    /// directly on the host.
    #[serde(rename = "source")]
    pub source: EscalateRequestLogSource,

    /// Subprocess-monotonic sequence number (uint64 as string — JTD has no
    /// native u64). Escape hatch for recovering subprocess-local order within
    /// a single source. Not authoritative across sources — use `host_ts` for
    /// merged-stream ordering.
    #[serde(rename = "source_seq")]
    pub source_seq: String,

    /// Subprocess wall-clock timestamp ISO8601 (advisory). Never used for
    /// ordering; the host stamps `host_ts` on receipt as the authoritative
    /// sort key.
    #[serde(rename = "source_ts")]
    pub source_ts: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateRequestReleaseHandle {
    /// Opaque handle ID previously returned by acquire_*.
    #[serde(rename = "handle_id")]
    pub handle_id: String,

    /// Correlates request with response. UUID string.
    #[serde(rename = "request_id")]
    pub request_id: String,
}
