// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/h264` — H.264 encoder + decoder via Vulkan Video, carved
//! out of the streamlib engine substrate.
//!
//! Linux-only today; macOS VideoToolbox H.264 is a future addition.

pub mod _generated_;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "linux")]
pub use linux::{H264DecoderProcessor, H264EncoderProcessor};

pub use _generated_::{H264DecoderConfig, H264EncoderConfig};

// `_apple_impl_pending_` holds the VideoToolbox encoder/decoder + the
// cross-platform Apple-flavored codec wrappers parked out of the engine
// in #786. Gated so it never compiles; re-enable + rewire imports to
// the public SDK surface once Apple support is activated.
#[cfg(any())]
mod _apple_impl_pending_;
