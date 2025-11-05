
// Public modules
pub mod host;
pub mod scanner;
pub mod plugin_info;
pub mod parameter_modulation;
pub mod parameter_automation;

// Re-export public types
pub use host::ClapPluginHost;
pub use scanner::{ClapScanner, ClapPluginInfo};
pub use plugin_info::{ParameterInfo, PluginInfo};
pub use parameter_modulation::{ParameterModulator, LfoWaveform};
pub use parameter_automation::{ParameterAutomation, ClapParameterControl};
