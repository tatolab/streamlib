// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated crate root — the mechanical projection of this package's
//! `processors/` directory. Do not edit: it is rewritten before every
//! cargo invocation and is excluded from the packed `.slpkg`.

#[allow(non_snake_case, unused_imports, dead_code, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

#[cfg(any())]
#[path = "../processors/_apple_impl_pending_/mod.rs"]
pub mod _apple_impl_pending_;
#[cfg(target_os = "linux")]
#[path = "../processors/camera_linux.rs"]
pub mod camera_linux;
#[path = "../processors/camera_to_cuda_copy.rs"]
pub mod camera_to_cuda_copy;
#[cfg(target_os = "linux")]
#[path = "../processors/v4l2_color_linux.rs"]
pub mod v4l2_color_linux;

streamlib_plugin_abi::export_plugin!(
    #[cfg(target_os = "linux")]
    crate::camera_linux::LinuxCameraProcessor::Processor,
    crate::camera_to_cuda_copy::CameraToCudaCopyProcessor::Processor,
);
