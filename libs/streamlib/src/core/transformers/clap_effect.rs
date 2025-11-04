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
    Result,
    StreamInput, StreamOutput,
};
use crate::core::frames::StereoSignal;
use crate::core::ports::PortMessage;
use crate::core::traits::{StreamElement, StreamProcessor, ElementType};
use crate::core::schema::PortDescriptor;
use crate::core::clap::{ClapPluginHost, ParameterInfo, PluginInfo};

use std::path::{Path, PathBuf};
use std::sync::Arc;
use serde::{Serialize, Deserialize};

/// Input ports for CLAP effect processors
pub struct ClapEffectInputPorts {
    pub audio: StreamInput<StereoSignal>,
}

/// Output ports for CLAP effect processors
pub struct ClapEffectOutputPorts {
    pub audio: StreamOutput<StereoSignal>,
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

    /// Sample rate (from RuntimeContext)
    sample_rate: u32,

    /// Buffer size (from RuntimeContext)
    buffer_size: usize,
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

    /// Process stereo signal samples through the plugin
    ///
    /// Takes samples from the input signal, processes them through CLAP, and returns a new signal.
    fn process_signal_through_clap(&mut self, input_signal: &StereoSignal, buffer_size: usize) -> Result<StereoSignal> {
        use crate::core::StreamError;
        use crate::core::frames::BufferGenerator;

        let host = self.host.as_mut()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?;

        // Take stereo samples from the input signal (interleaved: [L0, R0, L1, R1, ...])
        let input_samples = input_signal.take_interleaved(buffer_size);

        // Get plugin's internal buffers and process
        // The CLAP host expects deinterleaved buffers internally, which it handles
        // For now, we create a temporary AudioFrame for compatibility
        use crate::core::AudioFrame;
        let input_frame = AudioFrame::new(
            input_samples,
            input_signal.timestamp_ns(),
            0,
            2  // stereo
        );

        // Process through CLAP
        let output_frame = host.process_audio(&input_frame)?;

        // Convert output back to StereoSignal
        // Create stereo frames from interleaved samples
        let stereo_frames: Vec<[f32; 2]> = output_frame.samples
            .chunks_exact(2)
            .map(|chunk| [chunk[0], chunk[1]])
            .collect();

        // Wrap in a BufferGenerator and return as StereoSignal
        let generator = BufferGenerator::new(stereo_frames, false);
        Ok(StereoSignal::new(
            Box::new(generator),
            self.sample_rate,
            input_signal.timestamp_ns()
        ))
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
            schema: StereoSignal::schema(),
            required: true,
            description: "Stereo audio signal to process through CLAP plugin".to_string(),
        }]
    }

    fn output_ports(&self) -> Vec<PortDescriptor> {
        vec![PortDescriptor {
            name: "audio".to_string(),
            schema: StereoSignal::schema(),
            required: true,
            description: "Processed stereo audio signal from CLAP plugin".to_string(),
        }]
    }

    fn start(&mut self, ctx: &crate::core::RuntimeContext) -> Result<()> {
        use crate::core::StreamError;

        // Store runtime context values
        self.sample_rate = ctx.audio.sample_rate;
        self.buffer_size = ctx.audio.buffer_size;

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
            sample_rate: 48000,  // Default, overridden in start()
            buffer_size: 2048,   // Default, overridden in start()
        })
    }

    fn process(&mut self) -> Result<()> {
        tracing::debug!("[ClapEffect] process() called");

        // Read StereoSignal from input port
        if let Some(input_signal) = self.input_ports.audio.read_latest() {
            tracing::debug!("[ClapEffect] Got input signal, processing through CLAP");

            // Process through CLAP plugin (takes samples, processes, returns new StereoSignal)
            let output_signal = self.process_signal_through_clap(&input_signal, self.buffer_size)?;

            // Write output StereoSignal to output port
            self.output_ports.audio.write(output_signal);
            tracing::debug!("[ClapEffect] Wrote output signal");
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

    fn set_output_wakeup(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>) {
        if port_name == "audio" {
            self.output_ports.audio.set_downstream_wakeup(wakeup_tx);
        }
    }

    fn get_output_port_type(&self, port_name: &str) -> Option<crate::core::ports::PortType> {
        match port_name {
            "audio" => Some(self.output_ports.audio.port_type()),
            _ => None,
        }
    }

    fn get_input_port_type(&self, port_name: &str) -> Option<crate::core::ports::PortType> {
        match port_name {
            "audio" => Some(self.input_ports.audio.port_type()),
            _ => None,
        }
    }

    fn connect_bus_to_input(&mut self, port_name: &str, bus: Arc<dyn std::any::Any + Send + Sync>) -> bool {
        if port_name == "audio" {
            self.input_ports.audio.connect_bus(bus)
        } else {
            false
        }
    }

    fn create_bus_for_output(&self, port_name: &str) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        match port_name {
            "audio" => Some(self.output_ports.audio.get_or_create_bus() as Arc<dyn std::any::Any + Send + Sync>),
            _ => None,
        }
    }

    fn connect_bus_to_output(&mut self, port_name: &str, bus: Arc<dyn std::any::Any + Send + Sync>) -> bool {
        if let Some(typed_bus) = bus.downcast::<Arc<dyn crate::core::bus::Bus<StereoSignal>>>().ok() {
            if port_name == "audio" {
                self.output_ports.audio.set_bus(Arc::clone(&typed_bus));
                return true;
            }
        }
        false
    }

    fn connect_reader_to_input(&mut self, port_name: &str, reader: Box<dyn std::any::Any + Send>) -> bool {
        if let Ok(typed_reader) = reader.downcast::<Box<dyn crate::core::bus::BusReader<StereoSignal>>>() {
            if port_name == "audio" {
                self.input_ports.audio.connect_reader(*typed_reader);
                return true;
            }
        }
        false
    }
}

pub use crate::core::clap::{ClapScanner, ClapPluginInfo};
