// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.


use serde::{Deserialize, Serialize};

/// Polyglot subprocess escalate-on-behalf response (host → subprocess)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result")]
pub enum EscalateResponse {
    #[serde(rename = "contended")]
    Contended(EscalateResponseContended),

    #[serde(rename = "err")]
    Err(EscalateResponseErr),

    #[serde(rename = "ok")]
    Ok(EscalateResponseOk),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateResponseContended {
    /// Correlates response with request. Returned by
    /// [`try_acquire_cpu_readback`] (and any future `try_*` op that opts
    /// into the same shape) when the host's adapter would have blocked on
    /// a competing reader/writer. The subprocess gets no handle, no planes,
    /// and no surface-share registrations to release — `contended` is purely
    /// advisory, the customer skips the frame and re-tries later.
    #[serde(rename = "request_id")]
    pub request_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateResponseErr {
    /// Human-readable error message from the host side.
    #[serde(rename = "message")]
    pub message: String,

    /// Correlates response with request.
    #[serde(rename = "request_id")]
    pub request_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateResponseOkCpuReadbackPlane {
    /// Plane bytes-per-pixel. BGRA/RGBA: 4. NV12 plane 0 (Y): 1. NV12 plane 1
    /// (UV interleaved): 2.
    #[serde(rename = "bytes_per_pixel")]
    pub bytes_per_pixel: u32,

    /// Plane height in texels.
    #[serde(rename = "height")]
    pub height: u32,

    /// Surface-share UUID for this plane's staging buffer.
    #[serde(rename = "staging_surface_id")]
    pub staging_surface_id: String,

    /// Plane width in texels.
    #[serde(rename = "width")]
    pub width: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateResponseOk {
    /// Opaque handle returned by the host. For acquire_pixel_buffer this is
    /// the PixelBufferPoolId the host registered with its pixel-buffer pool and
    /// SurfaceStore. For acquire_texture this is a host-side UUID keying the
    /// EscalateHandleRegistry's texture slot. For release_handle this echoes
    /// the released id.
    #[serde(rename = "handle_id")]
    pub handle_id: String,

    /// Correlates response with request. Matches request_id in EscalateRequest.
    #[serde(rename = "request_id")]
    pub request_id: String,

    /// Per-plane staging-buffer descriptors set on `acquire_cpu_readback`
    /// responses. Length equals `SurfaceFormat::plane_count` for the
    /// target surface (1 for BGRA8/RGBA8, 2 for NV12). Each entry's
    /// `staging_surface_id` can be `check_out`ed from the surface-share
    /// service to obtain a DMA-BUF FD over the host-allocated staging
    /// `VulkanPixelBuffer` for that plane; mmap that FD to read or write the
    /// plane's tightly-packed bytes (`width * height * bytes_per_pixel` per
    /// plane).
    #[serde(rename = "cpu_readback_planes")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_readback_planes: Option<Vec<EscalateResponseOkCpuReadbackPlane>>,

    /// Resolved pixel or texture format identifier.
    #[serde(rename = "format")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,

    /// Height in pixels (set on acquire_pixel_buffer and acquire_texture
    /// responses).
    #[serde(rename = "height")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,

    /// Resolved usage tokens (set on acquire_texture responses). Array reflects
    /// the exact flags the host honored.
    #[serde(rename = "usage")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Vec<String>>,

    /// Width in pixels (set on acquire_pixel_buffer and acquire_texture
    /// responses).
    #[serde(rename = "width")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
}
