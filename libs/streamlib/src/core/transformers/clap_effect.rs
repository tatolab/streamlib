
use crate::core::{
    Result,
    StreamInput, StreamOutput,
};
use crate::core::frames::AudioFrame;
use crate::core::bus::PortMessage;
use crate::core::traits::{StreamElement, StreamProcessor, ElementType};
use crate::core::schema::PortDescriptor;
use crate::core::clap::{ClapPluginHost, ParameterInfo, PluginInfo};

use std::path::PathBuf;
use serde::{Serialize, Deserialize};

pub struct ClapEffectInputPorts {
    pub audio: StreamInput<AudioFrame<2>>,
}

pub struct ClapEffectOutputPorts {
    pub audio: StreamOutput<AudioFrame<2>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClapEffectConfig {
    pub plugin_path: PathBuf,
    pub plugin_name: Option<String>,
    pub plugin_index: Option<usize>,
}

impl Default for ClapEffectConfig {
    fn default() -> Self {
        Self {
            plugin_path: PathBuf::new(),
            plugin_name: None,
            plugin_index: None,
        }
    }
}

pub struct ClapEffectProcessor {
    config: ClapEffectConfig,

    host: Option<ClapPluginHost>,

    input_ports: ClapEffectInputPorts,

    output_ports: ClapEffectOutputPorts,

    sample_rate: u32,

    buffer_size: usize,
}

impl ClapEffectProcessor {
    pub fn plugin_info(&self) -> Result<&PluginInfo> {
        use crate::core::StreamError;
        self.host.as_ref()
            .map(|h| h.plugin_info())
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))
    }

    pub fn list_parameters(&self) -> Result<Vec<ParameterInfo>> {
        use crate::core::StreamError;
        self.host.as_ref()
            .map(|h| h.list_parameters())
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))
    }

    pub fn get_parameter(&self, id: u32) -> Result<f64> {
        use crate::core::StreamError;
        self.host.as_ref()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?
            .get_parameter(id)
    }

    pub fn set_parameter(&mut self, id: u32, value: f64) -> Result<()> {
        use crate::core::StreamError;
        self.host.as_mut()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?
            .set_parameter(id, value)
    }

    pub fn begin_edit(&mut self, id: u32) -> Result<()> {
        use crate::core::StreamError;
        self.host.as_mut()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?
            .begin_edit(id)
    }

    pub fn end_edit(&mut self, id: u32) -> Result<()> {
        use crate::core::StreamError;
        self.host.as_mut()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?
            .end_edit(id)
    }

    pub fn activate(&mut self, sample_rate: u32, max_frames: usize) -> Result<()> {
        use crate::core::StreamError;
        self.host.as_mut()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?
            .activate(sample_rate, max_frames)
    }

    pub fn deactivate(&mut self) -> Result<()> {
        use crate::core::StreamError;
        self.host.as_mut()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?
            .deactivate()
    }

    fn process_audio_through_clap(&mut self, input_frame: &AudioFrame<2>) -> Result<AudioFrame<2>> {
        use crate::core::StreamError;

        let host = self.host.as_mut()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?;

        let output_frame = host.process_audio(input_frame)?;

        Ok(output_frame)
    }

    pub fn input_ports(&mut self) -> &mut ClapEffectInputPorts {
        &mut self.input_ports
    }

    pub fn output_ports(&mut self) -> &mut ClapEffectOutputPorts {
        &mut self.output_ports
    }
}


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
            schema: AudioFrame::<2>::schema(),
            required: true,
            description: "Stereo audio frame to process through CLAP plugin".to_string(),
        }]
    }

    fn output_ports(&self) -> Vec<PortDescriptor> {
        vec![PortDescriptor {
            name: "audio".to_string(),
            schema: AudioFrame::<2>::schema(),
            required: true,
            description: "Processed stereo audio frame from CLAP plugin".to_string(),
        }]
    }

    fn start(&mut self, ctx: &crate::core::RuntimeContext) -> Result<()> {
        

        self.sample_rate = ctx.audio.sample_rate;
        self.buffer_size = ctx.audio.buffer_size;

        let mut host = if let Some(name) = self.config.plugin_name.as_deref() {
            ClapPluginHost::load_by_name(
                &self.config.plugin_path,
                name,
                ctx.audio.sample_rate,
                ctx.audio.buffer_size
            )?
        } else if let Some(index) = self.config.plugin_index {
            ClapPluginHost::load_by_index(
                &self.config.plugin_path,
                index,
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

        if let Some(input_frame) = self.input_ports.audio.read_latest() {
            tracing::debug!("[ClapEffect] Got input frame, processing through CLAP");

            let output_frame = self.process_audio_through_clap(&input_frame)?;

            self.output_ports.audio.write(output_frame);
            tracing::debug!("[ClapEffect] Wrote output frame");
        } else {
            tracing::debug!("[ClapEffect] No input available");
        }

        Ok(())
    }

    fn scheduling_config(&self) -> crate::core::scheduling::SchedulingConfig {
        use crate::core::scheduling::{SchedulingConfig, SchedulingMode, ThreadPriority};

        SchedulingConfig {
            mode: SchedulingMode::Push,
            priority: ThreadPriority::RealTime,
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

    fn get_output_port_type(&self, port_name: &str) -> Option<crate::core::bus::PortType> {
        match port_name {
            "audio" => Some(self.output_ports.audio.port_type()),
            _ => None,
        }
    }

    fn get_input_port_type(&self, port_name: &str) -> Option<crate::core::bus::PortType> {
        match port_name {
            "audio" => Some(self.input_ports.audio.port_type()),
            _ => None,
        }
    }

    fn wire_output_connection(&mut self, port_name: &str, connection: std::sync::Arc<dyn std::any::Any + Send + Sync>) -> bool {
        use crate::core::bus::ProcessorConnection;
        use crate::core::AudioFrame;

        if let Ok(typed_conn) = connection.downcast::<std::sync::Arc<ProcessorConnection<AudioFrame<2>>>>() {
            if port_name == "audio" {
                self.output_ports.audio.add_connection(std::sync::Arc::clone(&typed_conn));
                return true;
            }
        }
        false
    }

    fn wire_input_connection(&mut self, port_name: &str, connection: std::sync::Arc<dyn std::any::Any + Send + Sync>) -> bool {
        use crate::core::bus::ProcessorConnection;
        use crate::core::AudioFrame;

        if let Ok(typed_conn) = connection.downcast::<std::sync::Arc<ProcessorConnection<AudioFrame<2>>>>() {
            if port_name == "audio" {
                self.input_ports.audio.set_connection(std::sync::Arc::clone(&typed_conn));
                return true;
            }
        }
        false
    }
}

pub use crate::core::clap::{ClapScanner, ClapPluginInfo};
