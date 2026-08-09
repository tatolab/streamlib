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
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[path = "../processors/apple_unsupported.rs"]
pub mod apple_unsupported;
#[cfg(target_os = "linux")]
#[path = "../processors/display_linux.rs"]
pub mod display_linux;

#[cfg(target_os = "linux")]
streamlib_plugin_abi::export_plugin!(
    #[cfg(target_os = "linux")]
    crate::display_linux::LinuxDisplayProcessor::Processor,
);
