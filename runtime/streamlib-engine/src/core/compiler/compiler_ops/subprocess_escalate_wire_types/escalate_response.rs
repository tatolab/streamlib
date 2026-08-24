// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Responses the host writes back to a helper process over the escalate
//! bridge. Same wire contract as [`super::escalate_request`].

use serde::{Deserialize, Serialize};

/// Polyglot subprocess escalate-on-behalf response (host → subprocess)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result")]
pub(crate) enum EscalateResponse {
    #[serde(rename = "contended")]
    Contended(EscalateResponseContended),

    #[serde(rename = "err")]
    Err(EscalateResponseErr),

    #[serde(rename = "ok")]
    Ok(EscalateResponseOk),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateResponseContended {
    /// Correlates response with request. Returned by
    /// `try_run_cpu_readback_copy` (and any future `try_*` op that opts
    /// into the same shape) when another copy already holds the surface's
    /// staging. The subprocess gets no handle, no planes, and no
    /// surface-share registrations to release — `contended` is purely
    /// advisory, the customer skips the frame and re-tries later.
    pub(crate) request_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateResponseErr {
    /// Human-readable error message from the host side.
    pub(crate) message: String,

    /// Correlates response with request.
    pub(crate) request_id: String,
}

/// One binding of a registered kernel of any pipeline kind, as reflection
/// found it.
///
/// `kind` is the wire spelling rather than one of the three request enums,
/// because compute, graphics and ray tracing do not name the same set of kinds
/// and a caller only ever echoes this value back on the next dispatch. The
/// bytes are identical either way.
///
/// Stages are deliberately absent: a binding's stage visibility is settled at
/// construction, and no dispatch carries it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateResponseKernelBinding {
    /// Resource kind, in the same spelling the request's binding arrays use.
    pub(crate) kind: String,

    /// The shader's own name for the binding — what a dispatch supplies it by.
    pub(crate) name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateResponseOk {
    /// Opaque handle returned by the host. For acquire_pixel_buffer this is
    /// the surface-share check-in id (or, with no store, the acquisition's
    /// published frame id). For acquire_texture this is a host-side UUID keying the
    /// EscalateHandleRegistry's texture slot. For register_compute_kernel this
    /// is the SHA-256 hex of the SPIR-V blob — re-registering identical SPIR-V
    /// returns the same handle_id and re-uses the cached `VulkanComputeKernel`.
    /// For release_handle this echoes the released id. For run_compute_kernel
    /// this echoes the kernel_id (compute is synchronous host-side; nothing
    /// extra travels). For create_processor_owned_window this is the window
    /// id every other present-class op names; the other three echo the
    /// window id they were given.
    pub(crate) handle_id: String,

    /// Correlates response with request. Matches request_id in EscalateRequest.
    pub(crate) request_id: String,

    /// The kernel's binding shape as reflection found it, in slot order. Set on
    /// every `register_*_kernel` response.
    ///
    /// The caller needs it to dispatch: bindings resolve by name, and only the
    /// shader knows which kind each name is. Without this the caller would have
    /// to guess a kind for every binding it supplies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bindings: Option<Vec<EscalateResponseKernelBinding>>,

    /// Decimal-string-encoded u64 row pitch of the device-export staging,
    /// derived from the staging's own geometry rather than from the requesting
    /// surface's — the staging is the object the byte span was sized for. Set
    /// on `open_device_export_staging` responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bytes_per_row: Option<String>,

    /// Whether the user asked to close this window since the last drain. Set
    /// on `drain_processor_owned_window_events` responses, and true exactly
    /// once per gesture — the drain that reports it also clears it. An owner
    /// reacts to this; it cannot veto it, because the engine has already
    /// closed the window by the time the answer is written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) close_requested_by_user: Option<bool>,

    /// Lowercase hex of the exporting Vulkan device's
    /// `VkPhysicalDeviceIDProperties::deviceUUID` (32 characters, no
    /// separators). Set on `open_device_export_staging` responses. The external
    /// device API must import onto the GPU that owns the memory; matching
    /// this UUID is the entire device-binding contract, and falling through to
    /// device ordinal 0 corrupts silently on a multi-GPU host.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) exporting_device_uuid: Option<String>,

    /// Resolved pixel or texture format identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) format: Option<String>,

    /// Height in pixels (set on acquire_pixel_buffer and acquire_texture
    /// responses, and on `create_processor_owned_window` /
    /// `drain_processor_owned_window_events` responses, where it is the
    /// window's current drawable height rather than a surface's).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) height: Option<u32>,

    /// Whether the engine has closed this window. Set on
    /// `show_surface_on_processor_owned_window`,
    /// `drain_processor_owned_window_events` and
    /// `close_processor_owned_window` responses. Sticky once true, and the
    /// reason `show…` answers `ok` rather than an error after a close: a user
    /// gesture never takes a pipeline down.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) processor_owned_window_is_closed: Option<bool>,

    /// Decimal-string-encoded u64 byte size of the device-export staging
    /// buffer — the span an imported device pointer covers. Set on
    /// `open_device_export_staging` responses. JSON has no 64-bit integer; same
    /// decimal-string convention as `timeline_value`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) staging_byte_size: Option<String>,

    /// Decimal-string-encoded u64 timeline value the host signaled
    /// on the surface's shared timeline semaphore at end-of-submit.
    /// Set on `run_cpu_readback_copy` and `try_run_cpu_readback_copy`
    /// responses, and on `refill_device_export_staging` /
    /// `copy_device_export_staging_back_to_surface` responses, where the
    /// timeline is the staging's own `refill_done`. The subprocess waits on its
    /// imported `ConsumerVulkanTimelineSemaphore` for this value before reading
    /// or writing the staging buffer mapped at registration time. JSON has no
    /// 64-bit integer — wire form is decimal-string, parsed back to u64 on
    /// the subprocess side.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) timeline_value: Option<String>,

    /// Resolved usage tokens (set on acquire_texture responses). Array reflects
    /// the exact flags the host honored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) usage: Option<Vec<String>>,

    /// Width in pixels (set on acquire_pixel_buffer and acquire_texture
    /// responses, and on `create_processor_owned_window` /
    /// `drain_processor_owned_window_events` responses, where it is the
    /// window's current drawable width rather than a surface's).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) width: Option<u32>,

    /// Whether the host can honour a write-back for this export. Set on
    /// `open_device_export_staging` responses. True only when the surface's
    /// sole backing is its own pooled allocation: a frame its producer also
    /// published as a registered texture is still the producer's, and a
    /// texture-backed export has no write-back path at all. A subprocess
    /// that takes a write lock over a read-only export is refused rather
    /// than silently dropping the edit at unlock.
    ///
    /// This is the capability at open time, and it is revocable: pool slots
    /// keep their ids across reuse, so a slot that was the surface's sole
    /// backing here can be re-acquired by a texture-registering producer
    /// while this export is still held. The host re-tests when the
    /// write-back is asked for, and refuses then — so `true` promises the
    /// write-back was available, never that it still is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) writable: Option<bool>,
}
