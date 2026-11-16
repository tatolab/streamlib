// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only


#[allow(non_snake_case, unused_imports, dead_code, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[path = "../nodes/clap_effect.rs"]
pub mod clap_effect;
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[path = "../nodes/host.rs"]
pub mod host;
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[path = "../nodes/parameter_automation.rs"]
pub mod parameter_automation;
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[path = "../nodes/parameter_modulation.rs"]
pub mod parameter_modulation;
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[path = "../nodes/plugin_info.rs"]
pub mod plugin_info;
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[path = "../nodes/scanner.rs"]
pub mod scanner;

#[cfg(any(target_os = "macos", target_os = "ios"))]
streamlib_plugin_abi::export_plugin!(
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    crate::clap_effect::ClapEffectNode::Node,
);
