// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CLAP audio plugin processor — wraps `ClapPluginHost` with a streamlib
//! processor lifecycle. Polls the input mailbox on a dedicated audio
//! thread and dispatches converted stereo frames into the plugin.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use crate::_generated_::AudioFrame;
use streamlib::sdk::context::RuntimeContextFullAccess;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::InputMailboxes;
use streamlib::sdk::processors::ManualProcessor;
use streamlib_audio::{ProcessorAudioConverter, ProcessorAudioConverterTargetFormat};

use crate::host::ClapPluginHost;
use crate::parameter_automation::ClapParameterControl;
use crate::plugin_info::{ParameterInfo, PluginInfo};

/// Wrapper for InputMailboxes pointer that is Send.
/// SAFETY: InputMailboxes is Send, and we ensure the pointed-to data outlives
/// any thread that uses this pointer (polling thread is joined in teardown()).
struct SendableInputsPtr(*const InputMailboxes);

// SAFETY: InputMailboxes is Send, and we control the lifetime
unsafe impl Send for SendableInputsPtr {}

impl SendableInputsPtr {
    /// SAFETY: Caller must ensure the pointed-to data is still valid.
    unsafe fn get(&self) -> &InputMailboxes {
        &*self.0
    }
}

/// Wrapper for ProcessorAudioConverter pointer that is Send.
/// SAFETY: We ensure the pointed-to data outlives any thread that uses this pointer,
/// and only one thread accesses it.
struct SendableAudioConverterPtr(*mut ProcessorAudioConverter);

// SAFETY: Only one thread accesses it, and we join before drop
unsafe impl Send for SendableAudioConverterPtr {}

#[allow(clippy::mut_from_ref)]
impl SendableAudioConverterPtr {
    /// SAFETY: Caller must ensure the pointed-to data is still valid
    /// and no other thread is accessing it.
    unsafe fn get_mut(&self) -> &mut ProcessorAudioConverter {
        &mut *self.0
    }
}

/// Wrapper for ClapPluginHost pointer that is Send.
/// SAFETY: We ensure the pointed-to data outlives any thread that uses this pointer,
/// and only one thread accesses it between start() and teardown().
struct SendableClapHostPtr(*mut Option<ClapPluginHost>);

// SAFETY: Only one thread accesses it, and we join before drop
unsafe impl Send for SendableClapHostPtr {}

#[allow(clippy::mut_from_ref)]
impl SendableClapHostPtr {
    /// SAFETY: Caller must ensure the pointed-to data is still valid
    /// and no other thread is accessing it.
    unsafe fn get_mut(&self) -> &mut Option<ClapPluginHost> {
        &mut *self.0
    }
}

#[streamlib::sdk::processor("ClapEffect")]
pub struct ClapEffectProcessor {
    host: Option<ClapPluginHost>,
    buffer_size: usize,
    polling_thread: Option<std::thread::JoinHandle<()>>,
    stop_polling: Arc<AtomicBool>,
    audio: Option<ProcessorAudioConverter>,
}

impl ClapEffectProcessor::Processor {
    pub fn plugin_info(&self) -> Result<&PluginInfo> {
        self.host
            .as_ref()
            .map(|h| h.plugin_info())
            .ok_or_else(|| Error::Configuration("Plugin not initialized".into()))
    }

    pub fn list_parameters(&self) -> Result<Vec<ParameterInfo>> {
        self.host
            .as_ref()
            .map(|h| h.list_parameters())
            .ok_or_else(|| Error::Configuration("Plugin not initialized".into()))
    }

    pub fn get_parameter(&self, id: u32) -> Result<f64> {
        self.host
            .as_ref()
            .ok_or_else(|| Error::Configuration("Plugin not initialized".into()))?
            .get_parameter(id)
    }

    pub fn set_parameter(&mut self, id: u32, value: f64) -> Result<()> {
        self.host
            .as_mut()
            .ok_or_else(|| Error::Configuration("Plugin not initialized".into()))?
            .set_parameter(id, value)
    }

    pub fn begin_edit(&mut self, id: u32) -> Result<()> {
        self.host
            .as_mut()
            .ok_or_else(|| Error::Configuration("Plugin not initialized".into()))?
            .begin_edit(id)
    }

    pub fn end_edit(&mut self, id: u32) -> Result<()> {
        self.host
            .as_mut()
            .ok_or_else(|| Error::Configuration("Plugin not initialized".into()))?
            .end_edit(id)
    }

    pub fn activate(&mut self, sample_rate: u32, max_frames: usize) -> Result<()> {
        self.host
            .as_mut()
            .ok_or_else(|| Error::Configuration("Plugin not initialized".into()))?
            .activate(sample_rate, max_frames)
    }

    pub fn deactivate(&mut self) -> Result<()> {
        self.host
            .as_mut()
            .ok_or_else(|| Error::Configuration("Plugin not initialized".into()))?
            .deactivate()
    }
}

impl ManualProcessor for ClapEffectProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.buffer_size = self.config.buffer_size as usize;
        self.audio = Some(ProcessorAudioConverter::new());

        // Load CLAP plugin with placeholder sample_rate — activate() will set the real rate
        // when the first input frame arrives in the polling thread
        let host = if let Some(name) = self.config.plugin_name.as_deref() {
            ClapPluginHost::load_by_name(
                &self.config.plugin_path,
                name,
                48000,
                self.buffer_size,
            )?
        } else if let Some(index) = self.config.plugin_index {
            ClapPluginHost::load_by_index(
                &self.config.plugin_path,
                index as usize,
                48000,
                self.buffer_size,
            )?
        } else {
            ClapPluginHost::load(&self.config.plugin_path, 48000, self.buffer_size)?
        };

        tracing::info!(
            "[ClapEffect] Loaded plugin '{}' (activation deferred to first input frame)",
            host.plugin_info().name,
        );
        self.host = Some(host);
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.stop_polling.store(true, Ordering::SeqCst);

        if let Some(handle) = self.polling_thread.take() {
            let _ = handle.join();
        }

        // Safe to access self.host now — polling thread is joined
        if let Some(ref mut host) = self.host {
            let name = host.plugin_info().name.clone();
            match host.deactivate() {
                Ok(()) => {
                    tracing::info!("[ClapEffect] Deactivated plugin '{}'", name);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        } else {
            Ok(())
        }
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let stop_flag = Arc::clone(&self.stop_polling);
        stop_flag.store(false, Ordering::SeqCst);

        // SAFETY for all raw pointers:
        // 1. The polling thread is stopped in teardown() before self is dropped
        // 2. Only the polling thread accesses these after start() returns
        // 3. In Manual mode, no other code touches self.inputs/self.audio/self.host
        //    between start() and teardown()
        let audio = self.audio.as_mut().ok_or_else(|| {
            Error::Configuration(
                "audio converter not initialized — setup() must run before start()".into(),
            )
        })?;
        let inputs_ptr = SendableInputsPtr(&self.inputs as *const _);
        let audio_ptr = SendableAudioConverterPtr(audio as *mut _);
        let host_ptr = SendableClapHostPtr(&mut self.host as *mut _);
        let outputs = self.outputs.clone();
        let buffer_size = self.buffer_size;

        let polling_thread = std::thread::spawn(move || {
            let mut clap_activated = false;
            let mut frame_counter: u64 = 0;

            let target = ProcessorAudioConverterTargetFormat {
                sample_rate: None, // Don't resample — AudioOutput handles that
                channels: Some(2), // Stereo for CLAP
                buffer_size: Some(buffer_size),
            };

            while !stop_flag.load(Ordering::SeqCst) {
                let inputs = unsafe { inputs_ptr.get() };

                if !inputs.has_data("audio_in") {
                    std::thread::sleep(std::time::Duration::from_micros(500));
                    continue;
                }

                let input_frame: AudioFrame = match inputs.read("audio_in") {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::error!("[ClapEffect] Read failed: {}", e);
                        continue;
                    }
                };

                // Deferred activation on first frame
                if !clap_activated {
                    let host = unsafe { host_ptr.get_mut() };
                    if let Some(ref mut h) = host {
                        match h.activate(input_frame.sample_rate, buffer_size) {
                            Ok(()) => {
                                tracing::info!(
                                    "[ClapEffect] Activated plugin '{}' at {}Hz (from input)",
                                    h.plugin_info().name,
                                    input_frame.sample_rate,
                                );
                                clap_activated = true;
                            }
                            Err(e) => {
                                tracing::error!("[ClapEffect] Activation failed: {}", e);
                                continue;
                            }
                        }
                    }
                }

                // Convert (channels + rechunk) and process through CLAP
                let audio = unsafe { audio_ptr.get_mut() };
                match audio.convert(&input_frame, &target) {
                    Ok(frames) => {
                        let host = unsafe { host_ptr.get_mut() };
                        if let Some(ref mut h) = host {
                            for frame in frames {
                                match h.process_audio(&frame) {
                                    Ok(output) => {
                                        if let Err(e) = outputs.write("audio_out", &output) {
                                            tracing::error!("[ClapEffect] Write failed: {}", e);
                                        }
                                    }
                                    Err(e) => {
                                        tracing::error!("[ClapEffect] CLAP process failed: {}", e)
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => tracing::error!("[ClapEffect] Convert failed: {}", e),
                }

                frame_counter += 1;
            }

            tracing::info!(
                "[ClapEffect] Polling thread stopped after {} frames",
                frame_counter
            );
        });

        self.polling_thread = Some(polling_thread);
        Ok(())
    }
}

impl ClapParameterControl for ClapEffectProcessor::Processor {
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
