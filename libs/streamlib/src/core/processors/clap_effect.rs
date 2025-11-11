use crate::core::{Result, StreamInput, StreamOutput};
use crate::core::frames::AudioFrame;
use crate::core::clap::{ClapPluginHost, ParameterInfo, PluginInfo};
use streamlib_macros::StreamProcessor;

use std::path::PathBuf;
use serde::{Serialize, Deserialize};

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

#[derive(StreamProcessor)]
#[processor(
    mode = Push,
    description = "CLAP audio plugin processor with parameter control and automation"
)]
pub struct ClapEffectProcessor {
    #[input(description = "Stereo audio frame to process through CLAP plugin")]
    audio_in: StreamInput<AudioFrame<2>>,

    #[output(description = "Processed stereo audio frame from CLAP plugin")]
    audio_out: StreamOutput<AudioFrame<2>>,

    #[config]
    config: ClapEffectConfig,

    // Runtime state fields - auto-detected (no attribute needed)
    host: Option<ClapPluginHost>,
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

    // Lifecycle - auto-detected by macro
    fn on_start(&mut self, ctx: &crate::core::RuntimeContext) -> Result<()> {
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

    fn on_stop(&mut self) -> Result<()> {
        use crate::core::StreamError;

        let host = self.host.as_mut()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?;

        host.deactivate()?;
        tracing::info!("[ClapEffect] Deactivated plugin '{}'", host.plugin_info().name);
        Ok(())
    }

    // Business logic - called by macro-generated process()
    fn process(&mut self) -> Result<()> {
        tracing::debug!("[ClapEffect] process() called");

        if let Some(input_frame) = self.audio_in.read_latest() {
            tracing::debug!("[ClapEffect] Got input frame, processing through CLAP");

            let output_frame = self.process_audio_through_clap(&input_frame)?;

            self.audio_out.write(output_frame);
            tracing::debug!("[ClapEffect] Wrote output frame");
        } else {
            tracing::debug!("[ClapEffect] No input available");
        }

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

pub use crate::core::clap::{ClapScanner, ClapPluginInfo};
