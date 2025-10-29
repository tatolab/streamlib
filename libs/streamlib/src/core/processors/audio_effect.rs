//! Audio Effect Processor - Platform-agnostic audio plugin hosting
//!
//! This module defines the core traits and types for hosting audio effect plugins
//! (CLAP, VST3, etc.) within streamlib's processing pipeline.
//!
//! # Architecture
//!
//! ```text
//! User Code
//!    ↓
//! AudioEffectProcessor trait (platform-agnostic)
//!    ↓
//! ┌─────────────────┬──────────────────┐
//! │ ClapEffect      │  Vst3Effect      │  (implementations)
//! │ Processor       │  Processor       │
//! └─────────────────┴──────────────────┘
//!    ↓                    ↓
//! clack-host          clap-wrapper
//! ```
//!
//! # Design Principles
//!
//! 1. **Zero-copy audio** - Pass AudioFrame directly to plugins
//! 2. **Thread-safe** - Plugins run in audio thread, parameters from any thread
//! 3. **Format-agnostic** - Same API for CLAP, VST3, AU, etc.
//! 4. **Agent-friendly** - Simple parameter control for AI agents
//!
//! # Example
//!
//! ```ignore
//! use streamlib::{ClapEffectProcessor, AudioEffectProcessor};
//!
//! // Load a reverb plugin
//! let mut reverb = ClapEffectProcessor::load("path/to/reverb.clap")?;
//!
//! // List available parameters
//! for param in reverb.list_parameters() {
//!     println!("{}: {} (range: {} - {})",
//!         param.name, param.value, param.min, param.max);
//! }
//!
//! // Set room size to 80%
//! reverb.set_parameter_by_name("Room Size", 0.8)?;
//!
//! // In process() method:
//! let output_frame = reverb.process_audio(&input_frame)?;
//! ```

use crate::core::{AudioFrame, Result, StreamError, StreamProcessor};
use std::path::Path;

/// Information about a plugin parameter
#[derive(Debug, Clone)]
pub struct ParameterInfo {
    /// Parameter ID (stable across sessions)
    pub id: u32,

    /// Human-readable name
    pub name: String,

    /// Current value in parameter's native units (e.g., dB, Hz, %, etc.)
    /// NOT normalized! Use min/max to understand the range.
    pub value: f64,

    /// Minimum value in parameter's native units
    pub min: f64,

    /// Maximum value in parameter's native units
    pub max: f64,

    /// Default value in parameter's native units
    pub default: f64,

    /// Is this parameter automatable?
    pub is_automatable: bool,

    /// Is this parameter stepped (discrete values like enums)?
    pub is_stepped: bool,

    /// Is this parameter periodic (wraps around, like phase)?
    pub is_periodic: bool,

    /// Is this parameter hidden from UI?
    pub is_hidden: bool,

    /// Is this parameter read-only?
    pub is_readonly: bool,

    /// Is this parameter a bypass parameter?
    pub is_bypass: bool,

    /// Display string for current value (e.g., "12.5 dB", "440 Hz")
    pub display: String,
}

/// Information about an audio effect plugin
#[derive(Debug, Clone)]
pub struct PluginInfo {
    /// Plugin name
    pub name: String,

    /// Vendor/manufacturer
    pub vendor: String,

    /// Version string
    pub version: String,

    /// Plugin format (e.g., "CLAP", "VST3")
    pub format: String,

    /// Unique identifier
    pub id: String,

    /// Number of audio inputs
    pub num_inputs: u32,

    /// Number of audio outputs
    pub num_outputs: u32,
}

/// Platform-agnostic audio effect processor trait
///
/// Implementations provide plugin hosting for different formats (CLAP, VST3, AU).
/// All implementations must be thread-safe and support real-time audio processing.
pub trait AudioEffectProcessor: StreamProcessor {
    /// Load a plugin from a file path
    ///
    /// # Arguments
    ///
    /// * `path` - Path to plugin file (.clap, .vst3, .component, etc.)
    ///
    /// # Returns
    ///
    /// Loaded and initialized plugin instance
    fn load<P: AsRef<Path>>(path: P) -> Result<Self>
    where
        Self: Sized;

    /// Get plugin information
    fn plugin_info(&self) -> &PluginInfo;

    /// List all available parameters
    fn list_parameters(&self) -> Vec<ParameterInfo>;

    /// Get a parameter by ID
    ///
    /// # Arguments
    ///
    /// * `id` - Parameter ID
    ///
    /// # Returns
    ///
    /// Current parameter value (normalized 0.0-1.0)
    fn get_parameter(&self, id: u32) -> Result<f64>;

    /// Set a parameter by ID
    ///
    /// # Arguments
    ///
    /// * `id` - Parameter ID
    /// * `value` - New value (normalized 0.0-1.0)
    ///
    /// # Thread Safety
    ///
    /// This method is thread-safe and can be called from any thread.
    /// Parameter changes are queued and applied in the audio thread.
    fn set_parameter(&mut self, id: u32, value: f64) -> Result<()>;

    /// Get a parameter by name
    ///
    /// Convenience method that searches for parameter by name.
    /// Use `get_parameter()` with ID for better performance.
    fn get_parameter_by_name(&self, name: &str) -> Result<f64> {
        let params = self.list_parameters();
        let param = params
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| StreamError::Configuration(format!("Parameter '{}' not found", name)))?;
        self.get_parameter(param.id)
    }

    /// Set a parameter by name
    ///
    /// Convenience method that searches for parameter by name.
    /// Use `set_parameter()` with ID for better performance.
    fn set_parameter_by_name(&mut self, name: &str, value: f64) -> Result<()> {
        let params = self.list_parameters();
        let param = params
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| StreamError::Configuration(format!("Parameter '{}' not found", name)))?;
        self.set_parameter(param.id, value)
    }

    /// Process an audio frame through the plugin
    ///
    /// # Arguments
    ///
    /// * `input` - Input audio frame
    ///
    /// # Returns
    ///
    /// Processed output audio frame
    ///
    /// # Real-time Safety
    ///
    /// This method is called in the audio thread and must be real-time safe:
    /// - No memory allocation
    /// - No blocking operations
    /// - No system calls
    fn process_audio(&mut self, input: &AudioFrame) -> Result<AudioFrame>;

    /// Activate the plugin for processing
    ///
    /// Called before audio processing starts. Plugins may allocate
    /// resources and prepare for real-time processing.
    ///
    /// # Arguments
    ///
    /// * `sample_rate` - Sample rate in Hz
    /// * `max_frames` - Maximum number of frames per process() call
    fn activate(&mut self, sample_rate: u32, max_frames: usize) -> Result<()>;

    /// Deactivate the plugin
    ///
    /// Called when audio processing stops. Plugins may release resources.
    fn deactivate(&mut self) -> Result<()>;

    /// Begin editing a parameter (start parameter transaction)
    ///
    /// Signals to the plugin that parameter changes are about to occur. This allows
    /// the plugin to optimize processing and avoid audio glitches during multi-parameter
    /// updates.
    ///
    /// # Transaction Semantics
    ///
    /// Think of this as starting a database transaction for parameter changes:
    /// - Call `begin_edit(id)` before making changes
    /// - Make one or more `set_parameter()` calls
    /// - Call `end_edit(id)` when done
    ///
    /// # Agent Use Cases
    ///
    /// **Power Armor Audio Isolation Example:**
    /// ```ignore
    /// // Agent detects unknown sound, needs to isolate frequency band
    /// let filter = ClapEffectProcessor::load("eq.clap")?;
    ///
    /// // Start transaction - batching multiple parameter changes
    /// filter.begin_edit(FILTER_TYPE_PARAM)?;
    /// filter.begin_edit(CUTOFF_PARAM)?;
    /// filter.begin_edit(Q_PARAM)?;
    /// filter.begin_edit(GAIN_PARAM)?;
    ///
    /// // Configure band-pass filter for 2-4 kHz range
    /// filter.set_parameter(FILTER_TYPE_PARAM, BANDPASS)?;
    /// filter.set_parameter(CUTOFF_PARAM, 3000.0)?;  // 3 kHz center
    /// filter.set_parameter(Q_PARAM, 2.0)?;           // Narrow band
    /// filter.set_parameter(GAIN_PARAM, 12.0)?;       // +12 dB boost
    ///
    /// // Commit transaction - plugin applies all changes atomically
    /// filter.end_edit(FILTER_TYPE_PARAM)?;
    /// filter.end_edit(CUTOFF_PARAM)?;
    /// filter.end_edit(Q_PARAM)?;
    /// filter.end_edit(GAIN_PARAM)?;
    ///
    /// // Forward clean audio to ML node for classification
    /// let isolated_audio = filter.process_audio(&mic_frame)?;
    /// ml_node.classify(isolated_audio)?;
    /// ```
    ///
    /// # Benefits
    ///
    /// - **Atomic updates** - Prevents audio glitches from incremental changes
    /// - **Performance** - Plugin can optimize internal processing
    /// - **Embedded systems** - Critical for real-time processing on Jetson/ARM
    /// - **Agent coordination** - Clear begin/commit semantics for AI control
    ///
    /// # Arguments
    ///
    /// * `id` - Parameter ID to begin editing
    ///
    /// # Thread Safety
    ///
    /// This method is thread-safe and can be called from any thread.
    ///
    /// # Default Implementation
    ///
    /// The default implementation does nothing. Override if your plugin format
    /// supports parameter transactions (CLAP, VST3).
    fn begin_edit(&mut self, _id: u32) -> Result<()> {
        Ok(())
    }

    /// End editing a parameter (commit parameter transaction)
    ///
    /// Signals to the plugin that parameter changes are complete. The plugin
    /// can now apply all changes atomically and optimize its internal state.
    ///
    /// See `begin_edit()` for full documentation and examples.
    ///
    /// # Arguments
    ///
    /// * `id` - Parameter ID to finish editing
    ///
    /// # Thread Safety
    ///
    /// This method is thread-safe and can be called from any thread.
    ///
    /// # Default Implementation
    ///
    /// The default implementation does nothing. Override if your plugin format
    /// supports parameter transactions (CLAP, VST3).
    fn end_edit(&mut self, _id: u32) -> Result<()> {
        Ok(())
    }
}

/// Port names for AudioEffectProcessor
pub struct AudioEffectInputPorts {
    pub audio: String,
}

impl Default for AudioEffectInputPorts {
    fn default() -> Self {
        Self {
            audio: "audio".to_string(),
        }
    }
}

pub struct AudioEffectOutputPorts {
    pub audio: String,
}

impl Default for AudioEffectOutputPorts {
    fn default() -> Self {
        Self {
            audio: "audio".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parameter_info() {
        let param = ParameterInfo {
            id: 0,
            name: "Volume".to_string(),
            value: 0.5,
            min: 0.0,
            max: 1.0,
            default: 0.5,
            is_automatable: true,
            is_stepped: false,
            is_periodic: false,
            is_hidden: false,
            is_readonly: false,
            is_bypass: false,
            display: "-6.0 dB".to_string(),
        };

        assert_eq!(param.name, "Volume");
        assert_eq!(param.value, 0.5);
        assert!(param.is_automatable);
        assert!(!param.is_stepped);
        assert!(!param.is_readonly);
    }

    #[test]
    fn test_plugin_info() {
        let info = PluginInfo {
            name: "Test Reverb".to_string(),
            vendor: "Test Vendor".to_string(),
            version: "1.0.0".to_string(),
            format: "CLAP".to_string(),
            id: "com.example.reverb".to_string(),
            num_inputs: 2,
            num_outputs: 2,
        };

        assert_eq!(info.name, "Test Reverb");
        assert_eq!(info.format, "CLAP");
        assert_eq!(info.num_inputs, 2);
    }
}
