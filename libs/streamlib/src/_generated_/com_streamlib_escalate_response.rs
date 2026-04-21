// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.


use serde::{Deserialize, Serialize};

/// Polyglot subprocess escalate-on-behalf response (host → subprocess)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result")]
pub enum EscalateResponse {
    #[serde(rename = "err")]
    Err(EscalateResponseErr),

    #[serde(rename = "ok")]
    Ok(EscalateResponseOk),
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
    /// broker SurfaceStore. For release_handle this echoes the released id.
    #[serde(rename = "handle_id")]
    pub handle_id: String,

    /// Correlates response with request. Matches request_id in EscalateRequest.
    #[serde(rename = "request_id")]
    pub request_id: String,

    /// Resolved pixel format identifier.
    #[serde(rename = "format")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,

    /// Height in pixels (set on acquire_pixel_buffer responses).
    #[serde(rename = "height")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,

    /// Width in pixels (set on acquire_pixel_buffer responses).
    #[serde(rename = "width")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
}
