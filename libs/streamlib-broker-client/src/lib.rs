// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Wire helpers for the per-runtime surface-sharing Unix socket +
//! SCM_RIGHTS protocol.
//!
//! This crate is the single shared home for the `connect_to_broker` /
//! `send_request` / `send_message_with_fd` / `recv_message_with_fd` trio. It
//! is deliberately tiny — `libc` + `serde_json` only — so the two polyglot
//! cdylibs (`streamlib-python-native`, `streamlib-deno-native`) can depend
//! on it without dragging the runtime's transitive closure (vulkanalia,
//! tokio, winit, …) into their dep graphs. The runtime-internal service in
//! `streamlib::linux::surface_broker` consumes the same helpers on its
//! client-request path, so the wire format has exactly one source.
//!
//! The wire is: a 4-byte big-endian `u32` length prefix followed by a JSON
//! payload, with an optional `SCM_RIGHTS` ancillary fd attached to the
//! payload `sendmsg`. Fd ownership is unchanged by these helpers — callers
//! that `close` their fd after send still do so.

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::{connect_to_broker, recv_message_with_fd, send_message_with_fd, send_request};
