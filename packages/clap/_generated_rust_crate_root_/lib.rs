// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated crate root — the mechanical projection of this package's
//! `processors/` directory. Do not edit: it is rewritten before every
//! cargo invocation and is excluded from the packed `.slpkg`.

#[allow(non_snake_case, unused_imports, dead_code, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[path = "../processors/clap_effect.rs"]
pub mod clap_effect;
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[path = "../processors/host.rs"]
pub mod host;
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[path = "../processors/parameter_automation.rs"]
pub mod parameter_automation;
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[path = "../processors/parameter_modulation.rs"]
pub mod parameter_modulation;
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[path = "../processors/plugin_info.rs"]
pub mod plugin_info;
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[path = "../processors/scanner.rs"]
pub mod scanner;

#[cfg(any(target_os = "macos", target_os = "ios"))]
streamlib_plugin_abi::export_plugin!(
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    crate::clap_effect::ClapEffectProcessor::Processor,
);
