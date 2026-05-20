// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! MP4 file writer processors.

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "linux")]
pub use linux::mp4_writer::LinuxMp4WriterProcessor;

// `_apple_impl_pending_` references engine-internal Apple types
// (`PixelTransferSession`, `RuntimeContext::run_on_runtime_thread_blocking`)
// that the SDK does not expose. Gated so it never compiles; re-enable
// once the SDK ships an Apple platform surface.
#[cfg(any())]
mod _apple_impl_pending_;

pub use _generated_::LinuxMp4WriterConfig;

#[cfg(all(feature = "plugin", target_os = "linux"))]
streamlib_plugin_abi::export_plugin!(crate::LinuxMp4WriterProcessor::Processor);
