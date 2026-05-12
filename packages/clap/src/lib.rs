// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/clap` — CLAP audio plugin host processor for streamlib.
//!
//! Apple-only today (macOS / iOS). Linux CLAP support is a future ticket.

pub mod _generated_;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod clap_effect;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod host;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod parameter_automation;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod parameter_modulation;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod plugin_info;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod scanner;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use clap_effect::ClapEffectProcessor;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use host::ClapPluginHost;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use parameter_automation::{ClapParameterControl, ParameterAutomation};
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use parameter_modulation::{LfoWaveform, ParameterModulator};
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use plugin_info::{ParameterInfo, PluginInfo};
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use scanner::{ClapPluginInfo, ClapScanner};

pub use _generated_::ClapEffectConfig;
