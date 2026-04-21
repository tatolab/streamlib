// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.


use serde::{Deserialize, Serialize};

/// Polyglot subprocess escalate-on-behalf request (subprocess → host)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum EscalateRequest {
    #[serde(rename = "acquire_pixel_buffer")]
    AcquirePixelBuffer(EscalateRequestAcquirePixelBuffer),

    #[serde(rename = "acquire_texture")]
    AcquireTexture(EscalateRequestAcquireTexture),

    #[serde(rename = "release_handle")]
    ReleaseHandle(EscalateRequestReleaseHandle),
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
    /// Texture format identifier (rgba8_unorm, bgra8_unorm, rgba16_float, ...).
    #[serde(rename = "format")]
    pub format: String,

    /// Pixel height of the texture.
    #[serde(rename = "height")]
    pub height: u32,

    /// Correlates request with response. UUID string.
    #[serde(rename = "request_id")]
    pub request_id: String,

    /// Usage tokens (copy_src, copy_dst, texture_binding, storage_binding, render_attachment).
    #[serde(rename = "usage")]
    pub usage: Vec<String>,

    /// Pixel width of the texture.
    #[serde(rename = "width")]
    pub width: u32,
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
