// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

pub mod host;
pub mod parameter_automation;
pub mod parameter_modulation;
pub mod plugin_info;
pub mod scanner;

pub use host::ClapPluginHost;
pub use parameter_automation::{ClapParameterControl, ParameterAutomation};
pub use parameter_modulation::{LfoWaveform, ParameterModulator};
pub use plugin_info::{ParameterInfo, PluginInfo};
pub use scanner::{ClapPluginInfo, ClapScanner};
