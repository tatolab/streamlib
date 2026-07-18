// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Parked AVFoundation camera implementation — never compiled (gated
//! `#[cfg(any())]` at `crate::_apple_impl_pending_`). It still reaches the
//! `streamlib` engine facade + the facade-only `sdk::display_info` /
//! `sdk::rhi::PixelBufferRef` the engine-free plugin SDK does not surface on
//! Apple yet. Retained in-tree for the conversion follow-up; see
//! `src/lib.rs` for the unpark checklist.

pub mod camera;
pub mod corevideo_ffi;

pub use camera::{AppleCameraDevice, AppleCameraProcessor};
