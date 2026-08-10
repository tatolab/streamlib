// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Responses the host writes back to a helper process over the escalate
//! bridge. Same wire contract as [`super::escalate_request`].

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
    pub request_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateResponseErr {
    /// Human-readable error message from the host side.
    pub message: String,

    /// Correlates response with request.
    pub request_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalateResponseOk {
    /// Opaque handle returned by the host. For acquire_pixel_buffer this is
    /// the PixelBufferPoolId the host registered with its pixel-buffer pool and
    /// SurfaceStore. For acquire_texture this is a host-side UUID keying the
    /// EscalateHandleRegistry's texture slot. For register_compute_kernel this
    /// is the SHA-256 hex of the SPIR-V blob — re-registering identical SPIR-V
    /// returns the same handle_id and re-uses the cached `VulkanComputeKernel`.
    /// For release_handle this echoes the released id. For run_compute_kernel
    /// this echoes the kernel_id (compute is synchronous host-side; nothing
    /// extra travels).
    pub handle_id: String,

    /// Correlates response with request. Matches request_id in EscalateRequest.
    pub request_id: String,

    /// Decimal-string-encoded u64 row pitch of the device-export staging,
    /// derived from the staging's own geometry rather than from the requesting
    /// surface's — the staging is the object the byte span was sized for. Set
    /// on `open_device_export_staging` responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_per_row: Option<String>,

    /// Lowercase hex of the exporting Vulkan device's
    /// `VkPhysicalDeviceIDProperties::deviceUUID` (32 characters, no
    /// separators). Set on `open_device_export_staging` responses. The external
    /// device API must import onto the GPU that owns the memory; matching
    /// this UUID is the entire device-binding contract, and falling through to
    /// device ordinal 0 corrupts silently on a multi-GPU host.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exporting_device_uuid: Option<String>,

    /// Resolved pixel or texture format identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,

    /// Height in pixels (set on acquire_pixel_buffer and acquire_texture
    /// responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,

    /// Decimal-string-encoded u64 byte size of the device-export staging
    /// buffer — the span an imported device pointer covers. Set on
    /// `open_device_export_staging` responses. JTD has no native u64; same
    /// decimal-string convention as `timeline_value`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staging_byte_size: Option<String>,

    /// Decimal-string-encoded u64 timeline value the host signaled
    /// on the surface's shared timeline semaphore at end-of-submit.
    /// Set on `run_cpu_readback_copy` and `try_run_cpu_readback_copy`
    /// responses, and on `refill_device_export_staging` /
    /// `copy_device_export_staging_back_to_surface` responses, where the
    /// timeline is the staging's own `refill_done`. The subprocess waits on its
    /// imported `ConsumerVulkanTimelineSemaphore` for this value before reading
    /// or writing the staging buffer mapped at registration time. JTD has no
    /// native u64 — wire form is decimal-string, parsed back to u64 on the
    /// subprocess side.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeline_value: Option<String>,

    /// Resolved usage tokens (set on acquire_texture responses). Array reflects
    /// the exact flags the host honored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Vec<String>>,

    /// Width in pixels (set on acquire_pixel_buffer and acquire_texture
    /// responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,

    /// Whether the host can honour a write-back for this export. Set on
    /// `open_device_export_staging` responses. False for texture-backed
    /// exports, which are read-only by construction — a subprocess that takes
    /// a write lock over one is refused rather than silently dropping the edit
    /// at unlock.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writable: Option<bool>,
}
