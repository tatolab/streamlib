// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestCopyDeviceExportStagingBackToSurface {
    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// Same surface as `open_device_export_staging.surface_id`. Publishes
    /// a device-side edit: the host copies the staging buffer back into the
    /// surface's own allocation so every other holder observes it, and signals
    /// `refill_done` at end-of-submit. Refused when the surface's export is
    /// read-only — the write-back path belongs to surfaces whose only backing
    /// is their own pooled allocation. Answers with the signalled
    /// `timeline_value`, absent where the device exports no timeline.
    pub(crate) surface_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestOpenCpuReadbackStaging {
    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// The surface whose pixels the subprocess wants to read with the CPU.
    /// Same contract as `open_device_export_staging` — the host allocates the
    /// staging if the surface has none, registers it plus, where the device
    /// exports one, its `refill_done` timeline with the surface-share service,
    /// and answers with the id they
    /// are registered under — differing in one respect: this staging is
    /// HOST_VISIBLE and HOST_COHERENT, so the consumer maps and reads it
    /// directly instead of importing it into a device API. A surface can carry
    /// both at once; they are separate allocations under separate ids.
    /// The `ok` response carries `handle_id`, `width`, `height`, `format`,
    /// `staging_byte_size`, `bytes_per_row`, `writable`, and
    /// `exporting_device_uuid`. The staging fd and any timeline fd travel over
    /// the surface-share socket at check-out, never over this one.
    pub(crate) surface_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestOpenDeviceExportStaging {
    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// The surface whose pixels the subprocess wants in an external device
    /// API's dialect (CUDA today). The host allocates that surface's OPAQUE_FD
    /// device-export staging buffer if it has none, registers the staging plus,
    /// where the device exports one, its `refill_done` timeline with the
    /// surface-share service, and answers
    /// with the id they are registered under. Pool allocations are DMA-BUF-
    /// flavoured wherever the device exports DMA-BUF, external device APIs
    /// import OPAQUE_FD, and on NVIDIA one allocation cannot export both,
    /// which is why the staging exists at all.
    /// The `ok` response carries `handle_id` (the surface-share id to check
    /// out), `width`, `height`, `format`, `staging_byte_size`, `bytes_per_row`,
    /// `writable`, and `exporting_device_uuid`. The staging fd and any timeline
    /// fd travel over the surface-share socket at check-out, never over this
    /// one.
    pub(crate) surface_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRefillDeviceExportStaging {
    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// Same surface as `open_device_export_staging.surface_id`. The
    /// host resolves the blit source fresh, copies the surface's current
    /// pixels into the staging buffer, and signals a new value on the
    /// staging's `refill_done` timeline at end-of-submit. The response's
    /// `timeline_value` is what the subprocess waits for on its imported
    /// `ConsumerVulkanTimelineSemaphore` before reading the staging; it is
    /// absent where the device exports no timeline, the copy having retired on
    /// the host before the reply. No FDs travel on the wire.
    /// The source is resolved per refill and never cached: rotating producers
    /// re-register a different texture under the same surface id every frame,
    /// so a cached source blits the previous cycle's pixels.
    pub(crate) surface_id: String,
}

/// Which copy the host runs. `image_to_buffer` reads the named frame into the
/// staging; `buffer_to_image` publishes the staged edit back into the surface's
/// own pooled allocation, and is refused unless that same frame was read in
/// first. Which Vulkan copy each becomes is the engine's business and varies
/// with the surface's backing.
///
/// The host signals a new value on the staging's timeline at end-of-submit; the
/// subprocess waits on the timeline (through its imported
/// `ConsumerVulkanTimelineSemaphore`) before reading or releasing. No FDs travel
/// on the wire — only the timeline value the host signaled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum EscalateRequestRunCpuReadbackCopyDirection {
    #[serde(rename = "buffer_to_image")]
    BufferToImage,

    #[serde(rename = "image_to_buffer")]
    ImageToBuffer,

    #[serde(rename = "whole_buffer_to_image")]
    WholeBufferToImage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EscalateRequestRunCpuReadbackCopy {
    /// Which copy the host runs. `image_to_buffer` reads the named frame
    /// into the staging; `buffer_to_image` publishes the staged edit back
    /// into the surface's own pooled allocation, and is refused unless
    /// that same frame was read in first; `whole_buffer_to_image` publishes
    /// a staging the subprocess wrote in full, replacing every pixel, with
    /// no read first. Which Vulkan copy each becomes
    /// is the engine's business and varies with the surface's backing.
    ///
    /// The host signals a new value on the staging's timeline at
    /// end-of-submit; the subprocess waits on the timeline (through its
    /// imported `ConsumerVulkanTimelineSemaphore`) before reading or
    /// releasing. No FDs travel on the wire — only the timeline value the
    /// host signaled, absent where the device exports no timeline and the
    /// copy retired on the host before the reply.
    pub(crate) direction: EscalateRequestRunCpuReadbackCopyDirection,

    /// Correlates request with response. UUID string.
    pub(crate) request_id: String,

    /// Any surface id the engine can resolve — a published frame
    /// (`<slot>#<generation>`) or a registered texture's id. It is not
    /// parsed as an integer and carries no registration precondition: the
    /// host mints the CPU-readable staging on first ask.
    ///
    /// `direction: image_to_buffer` reads the frame into the staging;
    /// `buffer_to_image` publishes an edit of it back, and is refused
    /// unless a read happened first — an unfilled staging holds
    /// uninitialised memory, not an edit.
    ///
    /// The subprocess reaches the staging's memory through
    /// `open_cpu_readback_staging` plus the surface-share check-out that
    /// follows it; no fd travels on this socket.
    pub(crate) surface_id: String,
}
