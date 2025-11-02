//! CLAP Plugin Utilities
//!
//! This module provides CLAP-specific utilities for hosting audio plugins.
//! These are used by `ClapEffectProcessor` and other CLAP-related transformers.
//!
//! ## Contents
//!
//! - `plugin_info` - Plugin metadata types (ParameterInfo, PluginInfo)
//! - `parameter_modulation` - LFO and envelope generators for parameter automation
//! - `parameter_automation` - Scheduler for time-based parameter changes
//!
//! ## Usage
//!
//! ```rust,ignore
//! use streamlib::core::clap::{ParameterInfo, PluginInfo};
//!
//! let info = PluginInfo {
//!     name: "Reverb".to_string(),
//!     vendor: "MyCompany".to_string(),
//!     format: "CLAP".to_string(),
//!     // ...
//! };
//! ```

pub mod plugin_info;
pub mod parameter_modulation;
pub mod parameter_automation;

// Re-export common types
pub use plugin_info::{ParameterInfo, PluginInfo};
pub use parameter_modulation::{ParameterModulator, LfoWaveform};
pub use parameter_automation::{ParameterAutomation, ClapParameterControl};
