// TODO(@jonathan): CLAP host module has unused structs/fields (HostData, HostShared)
// Review if these are needed for future CLAP plugin features or can be removed
#![allow(dead_code)]

use clack_host::{
    bundle::PluginBundle,
    events::{event_types::ParamValueEvent, io::EventBuffer},
    host::HostInfo,
    plugin::PluginInstance,
    prelude::*,
    process::StartedPluginAudioProcessor,
    utils::{ClapId, Cookie},
};

use clack_extensions::params::{ParamInfoBuffer, PluginParams};

use super::scanner::ClapScanner;
use crate::core::clap::{ParameterInfo, PluginInfo};
use crate::core::{AudioFrame, Result, StreamError};

use parking_lot::Mutex as ParkingLotMutex;
use std::path::Path;
use std::sync::Arc;

struct SharedState {
    parameters: std::collections::HashMap<u32, f64>,
    parameter_generation: usize, // Incremented whenever parameters change

    parameter_info: Vec<ParameterInfo>,

    sample_rate: u32,

    max_frames: usize,
}

struct HostData {
    shared: Arc<ParkingLotMutex<SharedState>>,
}

impl HostData {
    fn new(shared: Arc<ParkingLotMutex<SharedState>>) -> Self {
        Self { shared }
    }
}

struct HostShared {
    state: Arc<ParkingLotMutex<SharedState>>,
}

impl HostHandlers for HostData {
    type Shared<'a> = HostShared;
    type MainThread<'a> = (); // No additional main thread data needed
    type AudioProcessor<'a> = (); // No additional audio processor data needed
}

impl<'a> SharedHandler<'a> for HostShared {
    fn request_restart(&self) {
        tracing::debug!("Plugin requested restart");
    }

    fn request_process(&self) {
        tracing::debug!("Plugin requested process");
    }

    fn request_callback(&self) {
        tracing::debug!("Plugin requested callback");
    }
}

pub struct ClapPluginHost {
    bundle: PluginBundle,

    plugin_id: std::ffi::CString,

    plugin_info: PluginInfo,

    instance: Arc<ParkingLotMutex<Option<PluginInstance<HostData>>>>,

    audio_processor: Arc<ParkingLotMutex<Option<StartedPluginAudioProcessor<HostData>>>>,

    shared_state: Arc<ParkingLotMutex<SharedState>>,

    is_activated: bool,

    sample_rate: u32,
    buffer_size: usize,

    deinterleave_buffers: Vec<Vec<f32>>,
    output_buffers: Vec<Vec<f32>>,

    last_parameter_generation: usize,
}

// SAFETY: ClapPluginHost is Send despite PluginBundle containing raw pointers
unsafe impl Send for ClapPluginHost {}

impl ClapPluginHost {
    pub fn load_by_name<P: AsRef<Path>>(
        path: P,
        plugin_name: &str,
        sample_rate: u32,
        buffer_size: usize,
    ) -> Result<Self> {
        Self::load_internal(path, Some(plugin_name), sample_rate, buffer_size)
    }

    pub fn load_by_index<P: AsRef<Path>>(
        path: P,
        index: usize,
        sample_rate: u32,
        buffer_size: usize,
    ) -> Result<Self> {
        Self::load_internal_with_filter(path, sample_rate, buffer_size, |mut descriptors| {
            descriptors.nth(index).ok_or_else(|| {
                StreamError::Configuration(format!("Plugin index {} not found in bundle", index))
            })
        })
    }

    pub fn load<P: AsRef<Path>>(path: P, sample_rate: u32, buffer_size: usize) -> Result<Self> {
        Self::load_internal(path, None, sample_rate, buffer_size)
    }

    fn load_internal<P: AsRef<Path>>(
        path: P,
        plugin_name: Option<&str>,
        sample_rate: u32,
        buffer_size: usize,
    ) -> Result<Self> {
        if let Some(name) = plugin_name {
            let name = name.to_string();
            let path_str = path.as_ref().display().to_string();

            Self::load_internal_with_filter(path, sample_rate, buffer_size, |descriptors| {
                let all_descs: Vec<_> = descriptors.collect();

                let all_names: Vec<String> = all_descs
                    .iter()
                    .filter_map(|desc| {
                        desc.name()
                            .and_then(|n| n.to_str().ok())
                            .map(|s| s.to_string())
                    })
                    .collect();

                for plugin_name in &all_names {
                    tracing::debug!("Found plugin in bundle: '{}'", plugin_name);
                }

                all_descs
                    .into_iter()
                    .find(|desc| {
                        desc.name()
                            .and_then(|n| n.to_str().ok())
                            .map(|n| n == name)
                            .unwrap_or(false)
                    })
                    .ok_or_else(|| {
                        let available = if all_names.is_empty() {
                            "none found".to_string()
                        } else {
                            all_names.join(", ")
                        };

                        StreamError::Configuration(format!(
                            "Plugin '{}' not found in bundle {}. Available plugins: [{}]",
                            name, path_str, available
                        ))
                    })
            })
        } else {
            Self::load_internal_with_filter(path, sample_rate, buffer_size, |mut descriptors| {
                descriptors.next().ok_or_else(|| {
                    StreamError::Configuration("CLAP plugin bundle contains no plugins".into())
                })
            })
        }
    }

    fn load_internal_with_filter<P, F>(
        path: P,
        sample_rate: u32,
        buffer_size: usize,
        filter: F,
    ) -> Result<Self>
    where
        P: AsRef<Path>,
        F: for<'a> FnOnce(
            clack_host::factory::PluginDescriptorsIter<'a>,
        ) -> Result<clack_host::factory::PluginDescriptor<'a>>,
    {
        let path = path.as_ref();

        let binary_path = ClapScanner::get_bundle_binary_path(path)?;

        // SAFETY: Loading CLAP plugins is inherently unsafe as it loads dynamic libraries
        let bundle = unsafe {
            PluginBundle::load(&binary_path).map_err(|e| {
                StreamError::Configuration(format!(
                    "Failed to load CLAP plugin from {:?}: {:?}",
                    path, e
                ))
            })?
        };

        let factory = bundle.get_plugin_factory().ok_or_else(|| {
            StreamError::Configuration("CLAP plugin has no plugin factory".into())
        })?;

        let descriptor = filter(factory.plugin_descriptors())?;

        let plugin_id = descriptor
            .id()
            .ok_or_else(|| StreamError::Configuration("Plugin descriptor has no ID".into()))?;

        let plugin_id_str = plugin_id
            .to_str()
            .ok()
            .ok_or_else(|| StreamError::Configuration("Invalid plugin ID".into()))?
            .to_string();

        let plugin_id_cstring = std::ffi::CString::new(plugin_id_str.clone()).map_err(|e| {
            StreamError::Configuration(format!("Failed to create CString from plugin ID: {}", e))
        })?;

        let plugin_info = PluginInfo {
            name: descriptor
                .name()
                .and_then(|s| s.to_str().ok())
                .unwrap_or("Unknown")
                .to_string(),
            vendor: descriptor
                .vendor()
                .and_then(|s| s.to_str().ok())
                .unwrap_or("Unknown")
                .to_string(),
            version: descriptor
                .version()
                .and_then(|s| s.to_str().ok())
                .unwrap_or("Unknown")
                .to_string(),
            format: "CLAP".to_string(),
            id: plugin_id_str,
            num_inputs: 2,  // Will be updated after activation
            num_outputs: 2, // Will be updated after activation
        };

        let shared_state = Arc::new(ParkingLotMutex::new(SharedState {
            parameters: std::collections::HashMap::new(),
            parameter_generation: 0,
            parameter_info: Vec::new(),
            sample_rate,
            max_frames: buffer_size,
        }));

        Ok(Self {
            plugin_info,
            bundle,
            plugin_id: plugin_id_cstring,
            instance: Arc::new(ParkingLotMutex::new(None)),
            audio_processor: Arc::new(ParkingLotMutex::new(None)),
            shared_state,
            is_activated: false,
            sample_rate,
            buffer_size,
            deinterleave_buffers: Vec::new(),
            output_buffers: Vec::new(),
            last_parameter_generation: 0,
        })
    }

    pub fn plugin_info(&self) -> &PluginInfo {
        &self.plugin_info
    }

    pub fn list_parameters(&self) -> Vec<ParameterInfo> {
        let state = self.shared_state.lock();
        state.parameter_info.clone()
    }

    pub fn get_parameter(&self, id: u32) -> Result<f64> {
        let state = self.shared_state.lock();
        state
            .parameters
            .get(&id)
            .copied()
            .ok_or_else(|| StreamError::Configuration(format!("Parameter ID {} not found", id)))
    }

    pub fn set_parameter(&mut self, id: u32, value: f64) -> Result<()> {
        let mut state = self.shared_state.lock();
        state.parameters.insert(id, value);
        state.parameter_generation = state.parameter_generation.wrapping_add(1);

        tracing::info!(
            "Parameter {} set to {} (will be sent during next process)",
            id,
            value
        );

        Ok(())
    }

    pub fn begin_edit(&mut self, id: u32) -> Result<()> {
        tracing::debug!("begin_edit({}) - placeholder (not yet implemented)", id);
        Ok(())
    }

    pub fn end_edit(&mut self, id: u32) -> Result<()> {
        tracing::debug!("end_edit({}) - placeholder (not yet implemented)", id);
        Ok(())
    }

    pub fn activate(&mut self, sample_rate: u32, max_frames: usize) -> Result<()> {
        if self.is_activated {
            tracing::debug!("Plugin already activated, skipping");
            return Ok(());
        }

        {
            let mut state = self.shared_state.lock();
            state.sample_rate = sample_rate;
            state.max_frames = max_frames;
        }

        let host_info = HostInfo::new(
            "streamlib",
            "Tato Lab",
            "https://github.com/tatolab/streamlib",
            "0.1.0",
        )
        .map_err(|e| StreamError::Configuration(format!("Failed to create host info: {:?}", e)))?;

        let shared_state_clone = Arc::clone(&self.shared_state);

        let mut instance = PluginInstance::<HostData>::new(
            |_| HostShared {
                state: Arc::clone(&shared_state_clone),
            },
            |_shared| (), // Main thread handler factory
            &self.bundle,
            &self.plugin_id,
            &host_info,
        )
        .map_err(|e| {
            StreamError::Configuration(format!("Failed to create plugin instance: {:?}", e))
        })?;

        {
            let mut main_thread_handle = instance.plugin_handle();
            let shared_handle = main_thread_handle.shared();

            if let Some(params_ext) = shared_handle.get_extension::<PluginParams>() {
                let param_count = params_ext.count(&mut main_thread_handle);

                tracing::info!("Plugin has {} parameters", param_count);

                let mut param_buffer = ParamInfoBuffer::new();
                let mut parameter_infos = Vec::new();

                for i in 0..param_count {
                    if let Some(param_info) =
                        params_ext.get_info(&mut main_thread_handle, i, &mut param_buffer)
                    {
                        let name = std::str::from_utf8(param_info.name)
                            .unwrap_or("Unknown")
                            .trim_end_matches('\0')
                            .to_string();

                        let value = params_ext
                            .get_value(&mut main_thread_handle, param_info.id)
                            .unwrap_or(param_info.default_value);

                        let flags = param_info.flags;
                        let is_automatable = flags
                            .contains(clack_extensions::params::ParamInfoFlags::IS_AUTOMATABLE);
                        let is_stepped =
                            flags.contains(clack_extensions::params::ParamInfoFlags::IS_STEPPED);
                        let is_periodic =
                            flags.contains(clack_extensions::params::ParamInfoFlags::IS_PERIODIC);
                        let is_hidden =
                            flags.contains(clack_extensions::params::ParamInfoFlags::IS_HIDDEN);
                        let is_readonly =
                            flags.contains(clack_extensions::params::ParamInfoFlags::IS_READONLY);
                        let is_bypass =
                            flags.contains(clack_extensions::params::ParamInfoFlags::IS_BYPASS);

                        let mut display_buf = vec![std::mem::MaybeUninit::new(0u8); 256];
                        let display = params_ext
                            .value_to_text(
                                &mut main_thread_handle,
                                param_info.id,
                                value,
                                &mut display_buf,
                            )
                            .ok()
                            .and_then(|bytes| std::str::from_utf8(bytes).ok())
                            .unwrap_or("")
                            .to_string();

                        parameter_infos.push(ParameterInfo {
                            id: param_info.id.get(),
                            name: name.clone(),
                            min: param_info.min_value,
                            max: param_info.max_value,
                            default: param_info.default_value,
                            value,
                            is_automatable,
                            is_stepped,
                            is_periodic,
                            is_hidden,
                            is_readonly,
                            is_bypass,
                            display,
                        });

                        tracing::debug!(
                            "Parameter {}: {} [ID={}] = {:.2} (range {:.2} to {:.2})",
                            i,
                            name,
                            param_info.id.get(),
                            value,
                            param_info.min_value,
                            param_info.max_value
                        );
                    }
                }

                self.shared_state.lock().parameter_info = parameter_infos;
            } else {
                tracing::warn!("Plugin does not support parameter extension");
            }
        }

        let audio_config = PluginAudioConfiguration {
            sample_rate: sample_rate as f64,
            min_frames_count: 1,
            max_frames_count: max_frames as u32,
        };

        let activated = instance.activate(|_, _| (), audio_config).map_err(|e| {
            StreamError::Configuration(format!("Failed to activate plugin: {:?}", e))
        })?;

        let audio_processor = activated.start_processing().map_err(|e| {
            StreamError::Configuration(format!("Failed to start audio processing: {:?}", e))
        })?;

        *self.instance.lock() = Some(instance);
        *self.audio_processor.lock() = Some(audio_processor);

        self.is_activated = true;
        self.sample_rate = sample_rate;
        self.buffer_size = max_frames;

        self.deinterleave_buffers.clear();
        self.output_buffers.clear();
        for _ in 0..2 {
            self.deinterleave_buffers.push(vec![0.0; max_frames]);
            self.output_buffers.push(vec![0.0; max_frames]);
        }

        tracing::info!(
            "✅ Activated CLAP plugin '{}' at {}Hz, {} max frames",
            self.plugin_info.name,
            sample_rate,
            max_frames
        );

        Ok(())
    }

    pub fn deactivate(&mut self) -> Result<()> {
        if !self.is_activated {
            return Ok(());
        }

        *self.audio_processor.lock() = None;

        *self.instance.lock() = None;

        self.is_activated = false;

        tracing::info!("✅ Deactivated CLAP plugin '{}'", self.plugin_info.name);

        Ok(())
    }

    pub fn process_audio(&mut self, input: &AudioFrame<2>) -> Result<AudioFrame<2>> {
        let num_samples = input.sample_count();

        for i in 0..num_samples {
            let base_idx = i * 2; // 2 channels (stereo)
            self.deinterleave_buffers[0][i] = input.samples[base_idx];
            self.deinterleave_buffers[1][i] = input.samples[base_idx + 1];
        }

        self.process_audio_channels_inplace(num_samples)?;

        let output_len = num_samples * 2;
        let mut output_samples = Vec::with_capacity(output_len);
        for i in 0..num_samples {
            output_samples.push(self.output_buffers[0][i]);
            output_samples.push(self.output_buffers[1][i]);
        }

        Ok(AudioFrame::new(
            output_samples,
            input.timestamp_ns,
            input.frame_number,
            input.sample_rate,
        ))
    }

    fn process_audio_channels_inplace(&mut self, num_samples: usize) -> Result<()> {
        let mut processor_guard = self.audio_processor.lock();
        let processor = processor_guard
            .as_mut()
            .ok_or_else(|| StreamError::Configuration("Audio processor not started".into()))?;

        let mut input_ports_base = AudioPorts::with_capacity(2, 1);
        let mut output_ports_base = AudioPorts::with_capacity(2, 1);

        let input_ports = input_ports_base.with_input_buffers(std::iter::once(AudioPortBuffer {
            latency: 0,
            channels: AudioPortBufferType::f32_input_only(
                self.deinterleave_buffers
                    .iter_mut()
                    .map(|buf| InputChannel {
                        buffer: &mut buf[0..num_samples],
                        is_constant: false,
                    }),
            ),
        }));

        let mut output_ports =
            output_ports_base.with_output_buffers(std::iter::once(AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_output_only(
                    self.output_buffers
                        .iter_mut()
                        .map(|buf| &mut buf[0..num_samples]),
                ),
            }));

        let state = self.shared_state.lock();
        let current_gen = state.parameter_generation;
        let has_param_changes = current_gen != self.last_parameter_generation;

        let input_events = if has_param_changes {
            let mut event_buffer = EventBuffer::with_capacity(state.parameters.len());
            for (param_id, value) in state.parameters.iter() {
                let event = ParamValueEvent::new(
                    0,
                    ClapId::new(*param_id),
                    Pckn::match_all(),
                    *value,
                    Cookie::empty(),
                );
                event_buffer.push(&event);
            }
            drop(state); // Release lock ASAP
            self.last_parameter_generation = current_gen;
            event_buffer
        } else {
            drop(state); // Release lock ASAP
            EventBuffer::with_capacity(0)
        };

        let input_events_ref = input_events.as_input();

        processor
            .process(
                &input_ports,
                &mut output_ports,
                &input_events_ref,
                &mut OutputEvents::void(),
                None,
                None,
            )
            .map_err(|e| StreamError::Runtime(format!("Plugin processing failed: {:?}", e)))?;

        Ok(())
    }
}
