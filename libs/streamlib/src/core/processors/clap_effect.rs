// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::clap::{ClapPluginHost, ParameterInfo, PluginInfo};
use crate::core::frames::AudioFrame;
use crate::core::{LinkInput, LinkOutput, Result, RuntimeContext};

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClapEffectConfig {
    pub plugin_path: PathBuf,
    pub plugin_name: Option<String>,
    pub plugin_index: Option<usize>,
    pub sample_rate: u32,
    pub buffer_size: usize,
}

impl Default for ClapEffectConfig {
    fn default() -> Self {
        Self {
            plugin_path: PathBuf::new(),
            plugin_name: None,
            plugin_index: None,
            sample_rate: 48000,
            buffer_size: 512,
        }
    }
}

#[crate::processor(
    execution = Reactive,
    description = "CLAP audio plugin processor with parameter control and automation"
)]
pub struct ClapEffectProcessor {
    #[crate::input(description = "Stereo audio frame to process through CLAP plugin")]
    audio_in: LinkInput<AudioFrame>,

    #[crate::output(description = "Processed stereo audio frame from CLAP plugin")]
    audio_out: Arc<LinkOutput<AudioFrame>>,

    #[crate::config]
    config: ClapEffectConfig,

    host: Option<ClapPluginHost>,
    sample_rate: u32,
    buffer_size: usize,
}

impl ClapEffectProcessor::Processor {
    pub fn plugin_info(&self) -> Result<&PluginInfo> {
        use crate::core::StreamError;
        self.host
            .as_ref()
            .map(|h| h.plugin_info())
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))
    }

    pub fn list_parameters(&self) -> Result<Vec<ParameterInfo>> {
        use crate::core::StreamError;
        self.host
            .as_ref()
            .map(|h| h.list_parameters())
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))
    }

    pub fn get_parameter(&self, id: u32) -> Result<f64> {
        use crate::core::StreamError;
        self.host
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?
            .get_parameter(id)
    }

    pub fn set_parameter(&mut self, id: u32, value: f64) -> Result<()> {
        use crate::core::StreamError;
        self.host
            .as_mut()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?
            .set_parameter(id, value)
    }

    pub fn begin_edit(&mut self, id: u32) -> Result<()> {
        use crate::core::StreamError;
        self.host
            .as_mut()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?
            .begin_edit(id)
    }

    pub fn end_edit(&mut self, id: u32) -> Result<()> {
        use crate::core::StreamError;
        self.host
            .as_mut()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?
            .end_edit(id)
    }

    pub fn activate(&mut self, sample_rate: u32, max_frames: usize) -> Result<()> {
        use crate::core::StreamError;
        self.host
            .as_mut()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?
            .activate(sample_rate, max_frames)
    }

    pub fn deactivate(&mut self) -> Result<()> {
        use crate::core::StreamError;
        self.host
            .as_mut()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?
            .deactivate()
    }

    fn process_audio_through_clap(&mut self, input_frame: &AudioFrame) -> Result<AudioFrame> {
        use crate::core::StreamError;

        let host = self
            .host
            .as_mut()
            .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?;

        let output_frame = host.process_audio(input_frame)?;

        Ok(output_frame)
    }
}

impl crate::core::Processor for ClapEffectProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let result = (|| {
            self.sample_rate = self.config.sample_rate;
            self.buffer_size = self.config.buffer_size;

            let mut host = if let Some(name) = self.config.plugin_name.as_deref() {
                ClapPluginHost::load_by_name(
                    &self.config.plugin_path,
                    name,
                    self.config.sample_rate,
                    self.config.buffer_size,
                )?
            } else if let Some(index) = self.config.plugin_index {
                ClapPluginHost::load_by_index(
                    &self.config.plugin_path,
                    index,
                    self.config.sample_rate,
                    self.config.buffer_size,
                )?
            } else {
                ClapPluginHost::load(
                    &self.config.plugin_path,
                    self.config.sample_rate,
                    self.config.buffer_size,
                )?
            };

            host.activate(self.config.sample_rate, self.config.buffer_size)?;

            tracing::info!(
                "[ClapEffect] Loaded and activated plugin '{}' at {} Hz with {} buffer size",
                host.plugin_info().name,
                self.config.sample_rate,
                self.config.buffer_size
            );

            self.host = Some(host);
            Ok(())
        })();
        std::future::ready(result)
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        use crate::core::StreamError;

        let result = (|| {
            let host = self
                .host
                .as_mut()
                .ok_or_else(|| StreamError::Configuration("Plugin not initialized".into()))?;

            host.deactivate()?;
            tracing::info!(
                "[ClapEffect] Deactivated plugin '{}'",
                host.plugin_info().name
            );
            Ok(())
        })();
        std::future::ready(result)
    }

    fn process(&mut self) -> Result<()> {
        tracing::debug!("[ClapEffect] process() called");

        if let Some(input_frame) = self.audio_in.read() {
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

impl crate::core::clap::ClapParameterControl for ClapEffectProcessor::Processor {
    fn set_parameter(&mut self, id: u32, value: f64) -> Result<()> {
        ClapEffectProcessor::Processor::set_parameter(self, id, value)
    }

    fn begin_edit(&mut self, id: u32) -> Result<()> {
        ClapEffectProcessor::Processor::begin_edit(self, id)
    }

    fn end_edit(&mut self, id: u32) -> Result<()> {
        ClapEffectProcessor::Processor::end_edit(self, id)
    }
}

pub use crate::core::clap::{ClapPluginInfo, ClapScanner};
