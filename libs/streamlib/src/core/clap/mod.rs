// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod host;
pub mod parameter_automation;
pub mod parameter_modulation;
pub mod plugin_info;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod scanner;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use host::ClapPluginHost;
pub use parameter_automation::{ClapParameterControl, ParameterAutomation};
pub use parameter_modulation::{LfoWaveform, ParameterModulator};
pub use plugin_info::{ParameterInfo, PluginInfo};
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use scanner::{ClapPluginInfo, ClapScanner};
