// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/debug-utilities` — utility processors for development,
//! demos, and rigorous-input testing (BgraFileSource, SimplePassthrough).

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

pub mod simple_passthrough;

#[cfg(target_os = "linux")]
pub mod bgra_file_source;

#[cfg(target_os = "linux")]
pub mod jpeg_bytes_source;

pub use simple_passthrough::SimplePassthroughProcessor;

#[cfg(target_os = "linux")]
pub use bgra_file_source::BgraFileSourceProcessor;

#[cfg(target_os = "linux")]
pub use jpeg_bytes_source::JpegBytesSourceProcessor;
