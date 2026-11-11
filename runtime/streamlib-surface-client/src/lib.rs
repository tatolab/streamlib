// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Wire helpers for the per-runtime surface-sharing service: a Unix socket
//! with SCM_RIGHTS fd passing on Linux, a raw Mach channel carrying port
//! rights on macOS.
//!
//! This crate is the single shared home for both wires. It is deliberately
//! tiny — `libc` + `serde_json`, plus `mach2` and the objc2 Foundation and Metal
//! bindings on macOS — so the polyglot
//! cdylibs (the wheel's helper-process surface client) can depend on it
//! without dragging the runtime's transitive closure (vulkanalia, tokio, …)
//! into their dep graphs. The runtime-internal service consumes
//! the same helpers on both sides of each wire, so each format has exactly
//! one source.
//!
//! The Linux wire is: a 4-byte big-endian `u32` length prefix followed by a
//! JSON payload, with zero or more `SCM_RIGHTS` ancillary fds attached to the
//! payload `sendmsg`. Multi-FD capacity covers DMA-BUFs with disjoint planes
//! (e.g. NV12 under DRM format modifiers with separate Y and UV allocations);
//! the ceiling is `MAX_DMA_BUF_PLANES`. Fd ownership is unchanged by these
//! helpers — callers that `close` their fds after send still do so.
//!
//! The macOS wire is one complex Mach message per request or reply: port
//! descriptors, then a length-prefixed JSON payload carrying the same verbs
//! and fields. No launchd plist or bundle: the service registers a dynamic
//! bootstrap name, and validates each sender by the audit token the kernel
//! appends.

mod display_importable_frame;
mod gpu_pool_claim_table_hand_over;
mod published_frame_id_grammar;

pub use display_importable_frame::{
    SURFACE_SHARE_DISPLAY_IMPORTS_HELD_FIELD, SURFACE_SHARE_FRAME_IMAGE_LAYOUT_FIELD,
    SURFACE_SHARE_IMPORT_DISPLAY_IMPORTABLE_FRAME_OP,
};

pub use gpu_pool_claim_table_hand_over::{
    GpuPoolClaimTableHandOver, GpuPoolClaimTableRowLeasedToASlot,
    SURFACE_SHARE_CLAIM_TABLE_OF_GPU_POOL_OP, SURFACE_SHARE_CLAIMANT_COLUMN_FIELD,
    SURFACE_SHARE_GPU_POOL_FIELD, SURFACE_SHARE_HOLDS_POOL_SLOT_IMPORT_FIELD,
    SURFACE_SHARE_IMPORT_POOL_SLOT_OP, SURFACE_SHARE_POOL_SLOT_ROW_FIELD,
    SURFACE_SHARE_POOL_SLOT_ROW_LEASE_NUMBER_FIELD, SURFACE_SHARE_POOL_SLOTS_FIELD,
    SURFACE_SHARE_RETIRED_POOL_SLOTS_FIELD, gpu_pool_uuid_of_its_wire_name,
    gpu_pool_uuid_wire_name, refusal_the_surface_share_service_answered,
};

pub use published_frame_id_grammar::{
    pool_slot_key_of_surface_id, published_frame_id_of_pool_slot,
    split_pool_slot_and_frame_generation,
};

/// The resource type a `check_out` answer names; a registration that names
/// none is a pixel buffer.
pub fn resource_type_of_a_check_out(response: &serde_json::Value) -> &str {
    response
        .get("resource_type")
        .and_then(|value| value.as_str())
        .unwrap_or("pixel_buffer")
}

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::{
    MAX_DMA_BUF_PLANES, MAX_SCM_RIGHTS_FDS, connect_to_surface_share_socket, recv_message_with_fds,
    send_message_with_fds, send_request_with_fds,
};

#[cfg(target_os = "linux")]
mod linux_surface_check_out;

#[cfg(target_os = "linux")]
pub use linux_surface_check_out::{
    CheckedOutPixelBuffer, CheckedOutPixelBufferMemory, CheckedOutSurface, CheckedOutTexture,
    CheckedOutTextureMemory, SURFACE_SHARE_PIXEL_PLANE_STRIDES_FIELD,
    SURFACE_SHARE_TEXTURE_IMAGE_USAGE_DEFAULT, SurfaceShareCheckOutFds,
    check_out_surface_on_surface_share, device_uuid_of_its_hex_spelling,
    publish_image_layout_on_surface_share, release_check_out_on_surface_share,
};

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::{
    MAX_SURFACE_SHARE_MACH_MESSAGE_JSON_BYTES, MAX_SURFACE_SHARE_MACH_MESSAGE_PORTS,
    OwnedMachPortSet, OwnedMachReceiveRight, OwnedMachSendRight, ReceivedSurfaceShareMachMessage,
    ReceivedSurfaceShareMachTraffic, SURFACE_SHARE_HAS_CONSUME_DONE_PORT,
    SURFACE_SHARE_HAS_PRODUCE_DONE_PORT, SURFACE_SHARE_MACH_CONNECT_MESSAGE_ID,
    SURFACE_SHARE_MACH_REPLY_MESSAGE_ID, SURFACE_SHARE_MACH_REQUEST_MESSAGE_ID,
    SURFACE_SHARE_MACH_SERVICE_ENVIRONMENT_VARIABLE, SURFACE_SHARE_OP_SIGNAL_CONSUME_DONE,
    SURFACE_SHARE_OP_TIMELINE_IMPORT_REFUSED, SurfaceShareMachMessageReceiveBuffer,
    SurfaceShareMachSenderAuditIdentity, SurfaceShareMachServiceConnection,
    check_in_surface_share_mach_service, receive_surface_share_mach_traffic,
    request_dead_name_notification, send_surface_share_mach_message,
};

#[cfg(target_os = "macos")]
mod metal_shared_event_mach_port;

#[cfg(target_os = "macos")]
pub use metal_shared_event_mach_port::{
    mach_send_right_of_metal_shared_event_handle, metal_shared_event_handle_of_mach_send_right,
};
