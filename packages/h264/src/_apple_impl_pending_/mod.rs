// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Parked Apple H.264 (and VideoToolbox-shared H.265) implementation.
//!
//! These files were carved out of the engine in #786 alongside the
//! Linux codec carve-out. They retain their engine-era `use crate::...`
//! imports and do not compile against the package's public-SDK-only
//! surface yet — when Apple support is activated for `@tatolab/h264`,
//! rewire the imports the same way the Linux side already did
//! (`packages/h264/src/linux/encoder.rs` is the reference).
//!
//! The directory is gated behind `#[cfg(any())]` in `lib.rs` so it
//! never compiles. See `packages/mp4/src/_apple_impl_pending_/` for
//! the same pattern.

pub mod core_codec;
pub mod videotoolbox;
