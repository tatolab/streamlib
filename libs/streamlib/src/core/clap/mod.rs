//! CLAP Plugin Infrastructure
//!
//! Provides reusable CLAP plugin hosting infrastructure and utilities.
//!
//! ## Architecture
//!
//! - **host** - Core CLAP plugin host (loading, lifecycle, parameters, processing)
//! - **scanner** - System-wide plugin discovery
//! - **buffer_conversion** - Audio format conversion (internal)
//! - **plugin_info** - Plugin metadata types
//! - **parameter_automation** - Time-based parameter automation
//! - **parameter_modulation** - LFO and envelope generators
//!
//! ## Public API
//!
//! ```rust,ignore
//! use streamlib::clap::{ClapPluginHost, ClapScanner, ParameterInfo};
//!
//! // Discover installed plugins
//! let plugins = ClapScanner::scan_system_plugins()?;
//!
//! // Load and use a plugin
//! let mut host = ClapPluginHost::load_by_name(
//!     &plugins[0].path,
//!     "Reverb",
//!     48000,
//!     2048
//! )?;
//!
//! host.activate(48000, 2048)?;
//! let output = host.process_audio(&input_frame)?;
//! ```
//!
//! ## Design Philosophy
//!
//! CLAP hosting infrastructure is separated from transformer logic to enable:
//! - Reusability across different transformer types
//! - Clear separation of concerns (hosting vs transformation)
//! - Testability of host logic independent of streamlib runtime

// Public modules
pub mod host;
pub mod scanner;
pub mod plugin_info;
pub mod parameter_modulation;
pub mod parameter_automation;

// Internal modules (not exposed in public API)
pub(crate) mod buffer_conversion;

// Re-export public types
pub use host::ClapPluginHost;
pub use scanner::{ClapScanner, ClapPluginInfo};
pub use plugin_info::{ParameterInfo, PluginInfo};
pub use parameter_modulation::{ParameterModulator, LfoWaveform};
pub use parameter_automation::{ParameterAutomation, ClapParameterControl};
