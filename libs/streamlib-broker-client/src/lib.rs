// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Wire helpers for the per-runtime surface-sharing Unix socket +
//! SCM_RIGHTS protocol.
//!
//! This crate is the single shared home for the `connect_to_broker` /
//! `send_request_with_fds` / `send_message_with_fds` / `recv_message_with_fds`
//! trio. It is deliberately tiny — `libc` + `serde_json` only — so the two
//! polyglot cdylibs (`streamlib-python-native`, `streamlib-deno-native`) can
//! depend on it without dragging the runtime's transitive closure
//! (vulkanalia, tokio, winit, …) into their dep graphs. The runtime-internal
//! service in `streamlib::linux::surface_broker` consumes the same helpers
//! on its client-request path, so the wire format has exactly one source.
//!
//! The wire is: a 4-byte big-endian `u32` length prefix followed by a JSON
//! payload, with zero or more `SCM_RIGHTS` ancillary fds attached to the
//! payload `sendmsg`. Multi-FD capacity covers DMA-BUFs with disjoint planes
//! (e.g. NV12 under DRM format modifiers with separate Y and UV allocations);
//! the ceiling is [`MAX_DMA_BUF_PLANES`]. Fd ownership is unchanged by these
//! helpers — callers that `close` their fds after send still do so.

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::{
    connect_to_broker, recv_message_with_fds, send_message_with_fds, send_request_with_fds,
    MAX_DMA_BUF_PLANES,
};
