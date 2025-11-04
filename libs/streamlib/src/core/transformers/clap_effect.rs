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
    Result, AudioFrame,
    StreamInput, StreamOutput,
};
use crate::core::traits::{StreamElement, StreamProcessor, ElementType};
use crate::core::schema::{PortDescriptor, SCHEMA_AUDIO_FRAME};
use crate::core::clap::{ClapPluginHost, ParameterInfo, PluginInfo};

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
}

impl Default for ClapEffectConfig {
    fn default() -> Self {
        Self {
            plugin_path: PathBuf::new(),
            plugin_name: None,
        }
    }
}

/// CLAP plugin effect processor
///
/// Hosts CLAP audio plugins and integrates them into streamlib's processing pipeline.
///
/// This is a thin wrapper over `ClapPluginHost` that provides StreamElement integration.
pub struct ClapEffectProcessor {
    /// Configuration (stored until start())
    config: ClapEffectConfig,

    /// CLAP plugin host (loaded in start())
    host: Option<ClapPluginHost>,

    /// Input ports (transformer-specific)
    input_ports: ClapEffectInputPorts,

    /// Output ports (transformer-specific)
    output_ports: ClapEffectOutputPorts,
}

impl ClapEffectProcessor {
    /// Get plugin metadata
    pub fn plugin_info(&self) -> Result<&PluginInfo> {
        use crate::core::StreamError;
        self.host.as_ref()
            .map(|h| h.plugin_info())
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))
    }

    /// List all parameters
    pub fn list_parameters(&self) -> Result<Vec<ParameterInfo>> {
        use crate::core::StreamError;
        self.host.as_ref()
            .map(|h| h.list_parameters())
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))
    }

    /// Get parameter value
    pub fn get_parameter(&self, id: u32) -> Result<f64> {
        use crate::core::StreamError;
        self.host.as_ref()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?
            .get_parameter(id)
    }

    /// Set parameter value (in native units)
    pub fn set_parameter(&mut self, id: u32, value: f64) -> Result<()> {
        use crate::core::StreamError;
        self.host.as_mut()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?
            .set_parameter(id, value)
    }

    /// Begin parameter edit transaction
    pub fn begin_edit(&mut self, id: u32) -> Result<()> {
        use crate::core::StreamError;
        self.host.as_mut()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?
            .begin_edit(id)
    }

    /// End parameter edit transaction
    pub fn end_edit(&mut self, id: u32) -> Result<()> {
        use crate::core::StreamError;
        self.host.as_mut()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?
            .end_edit(id)
    }

    /// Activate the plugin
    pub fn activate(&mut self, sample_rate: u32, max_frames: usize) -> Result<()> {
        use crate::core::StreamError;
        self.host.as_mut()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?
            .activate(sample_rate, max_frames)
    }

    /// Deactivate the plugin
    pub fn deactivate(&mut self) -> Result<()> {
        use crate::core::StreamError;
        self.host.as_mut()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?
            .deactivate()
    }

    /// Process audio through plugin (direct API, not using ports)
    ///
    /// This is useful for testing or direct usage outside of StreamElement runtime.
    pub fn process_audio(&mut self, input: &AudioFrame) -> Result<AudioFrame> {
        use crate::core::StreamError;
        self.host.as_mut()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?
            .process_audio(input)
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
        self.host.as_ref()
            .map(|h| h.plugin_info().name.as_str())
            .unwrap_or("ClapEffect")
    }

    fn element_type(&self) -> ElementType {
        ElementType::Transform
    }

    fn descriptor(&self) -> Option<crate::core::schema::ProcessorDescriptor> {
        <ClapEffectProcessor as StreamProcessor>::descriptor()
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
        use crate::core::StreamError;

        // Load the plugin with runtime context values
        let mut host = if let Some(name) = self.config.plugin_name.as_deref() {
            ClapPluginHost::load_by_name(
                &self.config.plugin_path,
                name,
                ctx.audio.sample_rate,
                ctx.audio.buffer_size
            )?
        } else {
            ClapPluginHost::load(
                &self.config.plugin_path,
                ctx.audio.sample_rate,
                ctx.audio.buffer_size
            )?
        };

        // Activate the plugin
        host.activate(ctx.audio.sample_rate, ctx.audio.buffer_size)?;

        tracing::info!(
            "[ClapEffect] Loaded and activated plugin '{}' at {} Hz with {} buffer size",
            host.plugin_info().name,
            ctx.audio.sample_rate,
            ctx.audio.buffer_size
        );

        self.host = Some(host);
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        use crate::core::StreamError;

        let host = self.host.as_mut()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?;

        host.deactivate()?;
        tracing::info!("[ClapEffect] Deactivated plugin '{}'", host.plugin_info().name);
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

impl StreamProcessor for ClapEffectProcessor {
    type Config = ClapEffectConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        Ok(Self {
            config,
            host: None,
            input_ports: ClapEffectInputPorts {
                audio: StreamInput::new("audio"),
            },
            output_ports: ClapEffectOutputPorts {
                audio: StreamOutput::new("audio"),
            },
        })
    }

    fn process(&mut self) -> Result<()> {
        use crate::core::StreamError;

        tracing::debug!("[ClapEffect] process() called");

        // Get host reference
        let host = self.host.as_mut()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?;

        // Read audio frame from input port
        if let Some(input_frame) = self.input_ports.audio.read_latest() {
            tracing::debug!(
                "[ClapEffect] Got input frame - {} samples, frame #{}",
                input_frame.sample_count(),
                input_frame.frame_number
            );

            // Process through CLAP plugin host
            let output_frame = host.process_audio(&input_frame)?;

            // Write to output port
            self.output_ports.audio.write(output_frame);
            tracing::debug!("[ClapEffect] Wrote output frame");
        } else {
            tracing::debug!("[ClapEffect] No input available");
        }

        Ok(())
    }

    fn scheduling_config(&self) -> crate::core::scheduling::SchedulingConfig {
        use crate::core::scheduling::{SchedulingConfig, SchedulingMode, ThreadPriority, ClockSource};

        SchedulingConfig {
            mode: SchedulingMode::Reactive,
            priority: ThreadPriority::RealTime,
            clock: ClockSource::Audio,
            provide_clock: false,
        }
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

    fn take_output_consumer(&mut self, port_name: &str) -> Option<crate::core::traits::PortConsumer> {
        if port_name == "audio" {
            self.output_ports.audio
                .consumer_holder()
                .lock()
                .take()
                .map(|consumer| crate::core::traits::PortConsumer::Audio(consumer))
        } else {
            None
        }
    }

    fn connect_input_consumer(&mut self, port_name: &str, consumer: crate::core::traits::PortConsumer) -> bool {
        if port_name == "audio" {
            match consumer {
                crate::core::traits::PortConsumer::Audio(c) => {
                    self.input_ports.audio.connect_consumer(c);
                    true
                }
                _ => false,
            }
        } else {
            false
        }
    }

    fn set_output_wakeup(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>) {
        if port_name == "audio" {
            self.output_ports.audio.set_downstream_wakeup(wakeup_tx);
        }
    }
}

pub use crate::core::clap::{ClapScanner, ClapPluginInfo};
