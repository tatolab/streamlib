// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/mp4` — MP4 file writer processors carved out of the
//! streamlib engine substrate.
//!
//! Linux today exposes [`LinuxMp4WriterProcessor`], an ffmpeg-driven
//! writer. The Apple AVAssetWriter implementation is preserved in
//! `_apple_impl_pending_/` but is gated off until the SDK exposes the
//! Apple platform surface it depends on (`PixelTransferSession` and
//! the `RuntimeContext::run_on_runtime_thread_blocking` Apple
//! workflow). The gate is `#[cfg(any())]` so the module never
//! compiles and the absence of a working Apple `Mp4WriterProcessor`
//! is the visible architectural call this carve-out surfaces.

pub mod _generated_;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "linux")]
pub use linux::mp4_writer::LinuxMp4WriterProcessor;

// Apple AVAssetWriter implementation preserved verbatim from the
// engine substrate. The module body references `PixelTransferSession`
// and the engine's `RuntimeContext` Apple-side affordances that the
// SDK does not currently expose. The `cfg(any())` gate keeps the
// source in tree as a reference for the design work that lands an
// Apple SDK surface and re-enables this implementation.
#[cfg(any())]
mod _apple_impl_pending_;

pub use _generated_::LinuxMp4WriterConfig;
