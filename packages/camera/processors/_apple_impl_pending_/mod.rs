// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(any())]

//! Parked AVFoundation camera implementation — the file-level `#![cfg(any())]`
//! above gates the whole directory so it never compiles on any target, which is
//! also what keeps folder-backed discovery from resolving its `#[processor]`
//! into the generated crate root. It still reaches the `streamlib` engine
//! facade + the facade-only `sdk::display_info` / `sdk::rhi::PixelBufferRef`
//! the engine-free plugin SDK does not surface on Apple yet.
//!
//! To unpark, once that surface ships: replace the `#![cfg(any())]` with the
//! Apple target predicate and re-add the objc2 / dispatch2 interop deps in
//! `Cargo.toml`. The crate root and its `export_plugin!` entry need no edit —
//! generation derives both from the reachable `#[processor]`.

pub mod camera;
pub mod corevideo_ffi;

pub use camera::{AppleCameraDevice, AppleCameraProcessor};
