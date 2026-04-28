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

    /// Resolved pixel or texture format identifier.
    #[serde(rename = "format")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,

    /// Height in pixels (set on acquire_pixel_buffer and acquire_texture
    /// responses).
    #[serde(rename = "height")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,

    /// Decimal-string-encoded u64 timeline value the host signaled on
    /// the surface's shared timeline semaphore at end-of-submit. Set on
    /// `run_cpu_readback_copy` and `try_run_cpu_readback_copy` responses.
    /// The subprocess waits on its imported `ConsumerVulkanTimelineSemaphore`
    /// for this value before reading or writing the staging buffer mapped at
    /// registration time. JTD has no native u64 — wire form is decimal-string,
    /// parsed back to u64 on the subprocess side.
    #[serde(rename = "timeline_value")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeline_value: Option<String>,

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
