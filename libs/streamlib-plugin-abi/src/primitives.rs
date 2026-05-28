// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Primitive ABI types — log levels, filter interest, opaque host handle.

use core::ffi::c_void;

/// Log level for tracing + iceoryx2-log emits. Matches
/// `tracing::Level` and `iceoryx2_log_types::LogLevel` orderings;
/// `Fatal` from iceoryx2 collapses to `Error`.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HostLogLevel {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
}

/// Filter interest returned by the host's `tracing_register_callsite`
/// callback. Matches `tracing-core`'s `Interest` semantics: `Never`
/// permanently disables a callsite; `Always` permanently enables;
/// `Sometimes` defers to per-event `tracing_enabled`.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HostInterest {
    Never = 0,
    Sometimes = 1,
    Always = 2,
}

/// Opaque host-owned state pointer. Threaded through every callback
/// as the first argument; the host derefs to its concrete service
/// table, the cdylib treats it as opaque.
pub type HostHandle = *const c_void;
