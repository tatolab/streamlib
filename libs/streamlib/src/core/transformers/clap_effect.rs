//! CLAP Audio Effect Processor
//!
//! Transformer that hosts CLAP audio plugins using the reusable `ClapPluginHost` infrastructure.
//!
//! # Architecture
//!
//! ```text
//! ClapEffectProcessor (Transformer)
//!   ├─ ClapPluginHost (Reusable infrastructure)
//!   │   ├─ Plugin loading & lifecycle
//!   │   ├─ Parameter management
//!   │   └─ Audio processing
//!   └─ Port structure (StreamElement integration)
//! ```
//!
//! # Example
//!
//! ```ignore
//! use streamlib::ClapEffectProcessor;
//!
//! // Load plugin
//! let mut reverb = ClapEffectProcessor::load_by_name(
//!     "/path/to/reverb.clap",
//!     "Reverb",
//!     48000,
//!     2048
//! )?;
//!
//! // Configure
//! reverb.activate(48000, 2048)?;
//! reverb.set_parameter_by_id(0, 0.8)?;
//!
//! // Process (via StreamElement)
//! reverb.process()?;
//! ```

use crate::core::{
    Result, StreamError, AudioFrame,
    StreamInput, StreamOutput,
};
use crate::core::traits::{StreamElement, StreamTransform, ElementType};
use crate::core::schema::{PortDescriptor, SCHEMA_AUDIO_FRAME};
use crate::core::clap::{ClapPluginHost, ClapScanner, ParameterInfo, PluginInfo, ClapPluginInfo};

use std::path::{Path, PathBuf};
use serde::{Serialize, Deserialize};

/// Input ports for CLAP effect processors
pub struct ClapEffectInputPorts {
    pub audio: StreamInput<AudioFrame>,
}

/// Output ports for CLAP effect processors
pub struct ClapEffectOutputPorts {
    pub audio: StreamOutput<AudioFrame>,
}

/// Configuration for CLAP effect processors
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClapEffectConfig {
    /// Path to the CLAP plugin file
    pub plugin_path: PathBuf,
    /// Optional plugin name (if bundle contains multiple)
    pub plugin_name: Option<String>,
    /// Sample rate for audio processing
    pub sample_rate: u32,
    /// Buffer size for audio processing
    pub buffer_size: usize,
}

impl Default for ClapEffectConfig {
    fn default() -> Self {
        Self {
            plugin_path: PathBuf::new(), // Empty path - must be set by user
            plugin_name: None,
            sample_rate: 48000,  // Standard audio sample rate
            buffer_size: 2048,   // Standard CLAP buffer size
        }
    }
}

/// CLAP plugin effect processor
///
/// Hosts CLAP audio plugins and integrates them into streamlib's processing pipeline.
///
/// This is a thin wrapper over `ClapPluginHost` that provides StreamElement integration.
pub struct ClapEffectProcessor {
    /// CLAP plugin host (does all the heavy lifting)
    host: ClapPluginHost,

    /// Input ports (transformer-specific)
    input_ports: ClapEffectInputPorts,

    /// Output ports (transformer-specific)
    output_ports: ClapEffectOutputPorts,
}

impl ClapEffectProcessor {
    /// Load a specific plugin by name from a CLAP bundle
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the CLAP plugin bundle
    /// * `plugin_name` - Name of the plugin to load (case-sensitive)
    /// * `sample_rate` - Sample rate for audio processing
    /// * `buffer_size` - Buffer size for audio processing
    ///
    /// # Example
    ///
    /// ```ignore
    /// let plugin = ClapEffectProcessor::load_by_name(
    ///     "/path/to/bundle.clap",
    ///     "Gain",
    ///     48000,
    ///     512
    /// )?;
    /// ```
    pub fn load_by_name<P: AsRef<Path>>(
        path: P,
        plugin_name: &str,
        sample_rate: u32,
        buffer_size: usize
    ) -> Result<Self> {
        let host = ClapPluginHost::load_by_name(path, plugin_name, sample_rate, buffer_size)?;

        Ok(Self {
            host,
            input_ports: ClapEffectInputPorts {
                audio: StreamInput::new("audio".to_string()),
            },
            output_ports: ClapEffectOutputPorts {
                audio: StreamOutput::new("audio".to_string()),
            },
        })
    }

    /// Load a plugin by index from a CLAP bundle
    pub fn load_by_index<P: AsRef<Path>>(
        path: P,
        index: usize,
        sample_rate: u32,
        buffer_size: usize
    ) -> Result<Self> {
        let host = ClapPluginHost::load_by_index(path, index, sample_rate, buffer_size)?;

        Ok(Self {
            host,
            input_ports: ClapEffectInputPorts {
                audio: StreamInput::new("audio".to_string()),
            },
            output_ports: ClapEffectOutputPorts {
                audio: StreamOutput::new("audio".to_string()),
            },
        })
    }

    /// Load the first plugin from a CLAP bundle
    pub fn load<P: AsRef<Path>>(path: P, sample_rate: u32, buffer_size: usize) -> Result<Self> {
        let host = ClapPluginHost::load(path, sample_rate, buffer_size)?;

        Ok(Self {
            host,
            input_ports: ClapEffectInputPorts {
                audio: StreamInput::new("audio".to_string()),
            },
            output_ports: ClapEffectOutputPorts {
                audio: StreamOutput::new("audio".to_string()),
            },
        })
    }

    /// Get plugin metadata
    pub fn plugin_info(&self) -> &PluginInfo {
        self.host.plugin_info()
    }

    /// List all parameters
    pub fn list_parameters(&self) -> Vec<ParameterInfo> {
        self.host.list_parameters()
    }

    /// Get parameter value
    pub fn get_parameter(&self, id: u32) -> Result<f64> {
        self.host.get_parameter(id)
    }

    /// Set parameter value (in native units)
    pub fn set_parameter(&mut self, id: u32, value: f64) -> Result<()> {
        self.host.set_parameter(id, value)
    }

    /// Begin parameter edit transaction
    pub fn begin_edit(&mut self, id: u32) -> Result<()> {
        self.host.begin_edit(id)
    }

    /// End parameter edit transaction
    pub fn end_edit(&mut self, id: u32) -> Result<()> {
        self.host.end_edit(id)
    }

    /// Activate the plugin
    pub fn activate(&mut self, sample_rate: u32, max_frames: usize) -> Result<()> {
        self.host.activate(sample_rate, max_frames)
    }

    /// Deactivate the plugin
    pub fn deactivate(&mut self) -> Result<()> {
        self.host.deactivate()
    }

    /// Process audio through plugin (direct API, not using ports)
    ///
    /// This is useful for testing or direct usage outside of StreamElement runtime.
    pub fn process_audio(&mut self, input: &AudioFrame) -> Result<AudioFrame> {
        self.host.process_audio(input)
    }

    /// Get input ports
    pub fn input_ports(&mut self) -> &mut ClapEffectInputPorts {
        &mut self.input_ports
    }

    /// Get output ports
    pub fn output_ports(&mut self) -> &mut ClapEffectOutputPorts {
        &mut self.output_ports
    }
}

// ============================================================
// StreamElement Implementation (v2.0.0 Architecture)
// ============================================================

impl StreamElement for ClapEffectProcessor {
    fn name(&self) -> &str {
        &self.host.plugin_info().name
    }

    fn element_type(&self) -> ElementType {
        ElementType::Transform
    }

    fn descriptor(&self) -> Option<crate::core::schema::ProcessorDescriptor> {
        <ClapEffectProcessor as StreamTransform>::descriptor()
    }

    fn input_ports(&self) -> Vec<PortDescriptor> {
        vec![PortDescriptor {
            name: "audio".to_string(),
            schema: SCHEMA_AUDIO_FRAME.clone(),
            required: true,
            description: "Audio input to process through CLAP plugin".to_string(),
        }]
    }

    fn output_ports(&self) -> Vec<PortDescriptor> {
        vec![PortDescriptor {
            name: "audio".to_string(),
            schema: SCHEMA_AUDIO_FRAME.clone(),
            required: true,
            description: "Processed audio output from CLAP plugin".to_string(),
        }]
    }

    fn start(&mut self, ctx: &crate::core::RuntimeContext) -> Result<()> {
        // Activate the plugin with context's audio configuration
        self.host.activate(ctx.audio.sample_rate, ctx.audio.buffer_size)?;
        tracing::info!(
            "[ClapEffect] Activated plugin '{}' at {} Hz with {} samples buffer",
            self.host.plugin_info().name,
            ctx.audio.sample_rate,
            ctx.audio.buffer_size
        );
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.host.deactivate()?;
        tracing::info!("[ClapEffect] Deactivated plugin '{}'", self.host.plugin_info().name);
        Ok(())
    }
}

// ============================================================
// ClapParameterControl Implementation (for automation)
// ============================================================

impl crate::core::clap::ClapParameterControl for ClapEffectProcessor {
    fn set_parameter(&mut self, id: u32, value: f64) -> Result<()> {
        self.set_parameter(id, value)
    }

    fn begin_edit(&mut self, id: u32) -> Result<()> {
        self.begin_edit(id)
    }

    fn end_edit(&mut self, id: u32) -> Result<()> {
        self.end_edit(id)
    }
}

// ============================================================
// StreamTransform Implementation (v2.0.0 Architecture)
// ============================================================

impl StreamTransform for ClapEffectProcessor {
    type Config = ClapEffectConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        if let Some(name) = config.plugin_name.as_deref() {
            Self::load_by_name(&config.plugin_path, name, config.sample_rate, config.buffer_size)
        } else {
            Self::load(&config.plugin_path, config.sample_rate, config.buffer_size)
        }
    }

    fn process(&mut self) -> Result<()> {
        tracing::debug!("[ClapEffect] process() called");

        // Read audio frame from input port
        if let Some(input_frame) = self.input_ports.audio.read_latest() {
            tracing::debug!(
                "[ClapEffect] Got input frame - {} samples, frame #{}",
                input_frame.sample_count(),
                input_frame.frame_number
            );

            // Process through CLAP plugin host
            let output_frame = self.host.process_audio(&input_frame)?;

            // Write to output port
            self.output_ports.audio.write(output_frame);
            tracing::debug!("[ClapEffect] Wrote output frame");
        } else {
            tracing::debug!("[ClapEffect] No input available");
        }

        Ok(())
    }

    fn descriptor() -> Option<crate::core::schema::ProcessorDescriptor> {
        use crate::core::schema::{ProcessorDescriptor, AudioRequirements};

        Some(
            ProcessorDescriptor::new(
                "ClapEffectProcessor",
                "CLAP audio plugin processor with parameter control and automation"
            )
            .with_usage_context(
                "Use for loading and processing audio through CLAP plugins. \
                 Supports parameter enumeration, modification, transactions (begin_edit/end_edit), \
                 and automation. Most CLAP plugins require stereo input at specific buffer sizes."
            )
            .with_audio_requirements(AudioRequirements {
                preferred_buffer_size: Some(2048),  // Standard CLAP buffer size
                required_buffer_size: Some(2048),    // Many plugins require this
                supported_sample_rates: vec![44100, 48000, 96000],  // Common rates
                required_channels: Some(2),          // Most plugins expect stereo
            })
            .with_tags(vec!["audio", "effect", "clap", "plugin", "transform"])
        )
    }
}

// Re-export scanner for convenience
pub use crate::core::clap::scanner::{ClapScanner, ClapPluginInfo};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clap_effect_load_by_name() {
        let plugin_path = "/Users/fonta/Repositories/tatolab/clap-plugins/build/plugins/clap-plugins.clap/Contents/MacOS/clap-plugins";

        if !std::path::Path::new(plugin_path).exists() {
            eprintln!("Skipping test - CLAP plugin not found");
            return;
        }

        let processor = ClapEffectProcessor::load_by_name(plugin_path, "Gain", 48000, 512);
        assert!(processor.is_ok());

        let processor = processor.unwrap();
        assert_eq!(processor.plugin_info().name, "Gain");
        assert_eq!(processor.plugin_info().format, "CLAP");
    }

    #[test]
    fn test_clap_effect_process() {
        let plugin_path = "/Users/fonta/Repositories/tatolab/clap-plugins/build/plugins/clap-plugins.clap/Contents/MacOS/clap-plugins";

        if !std::path::Path::new(plugin_path).exists() {
            eprintln!("Skipping test - CLAP plugin not found");
            return;
        }

        let mut processor = ClapEffectProcessor::load_by_name(plugin_path, "Gain", 48000, 512)
            .expect("Failed to load plugin");

        processor.activate(48000, 512).expect("Failed to activate");

        // Create test audio
        let samples = vec![0.5f32; 512 * 2]; // 512 samples stereo
        let input_frame = AudioFrame::new(samples, 0, 0, 2);

        let output_frame = processor.process_audio(&input_frame).expect("Failed to process");

        assert_eq!(output_frame.channels, 2);
        assert_eq!(output_frame.sample_count(), 512);

        processor.deactivate().expect("Failed to deactivate");
    }
}
