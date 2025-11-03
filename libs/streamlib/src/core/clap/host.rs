//! CLAP plugin host infrastructure
//!
//! Provides reusable CLAP plugin hosting functionality that can be used by any transformer.
//! Handles plugin loading, lifecycle management, parameter control, and audio processing.
//!
//! # Architecture
//!
//! ```text
//! ClapPluginHost
//!   ├─ Plugin Bundle (loaded .clap file)
//!   ├─ Plugin Instance (activated processor)
//!   ├─ Shared State (parameters, configuration)
//!   └─ Audio Processor (real-time processing)
//! ```
//!
//! # Thread Safety
//!
//! The host supports both `Arc<Mutex<T>>` and `RefCell<T>` synchronization via generic parameter:
//! - `ClapPluginHost<Arc<Mutex<_>>>` - Thread-safe, can be sent across threads
//! - `ClapPluginHost<RefCell<_>>` - Single-threaded, faster but not Send
//!
//! # Example
//!
//! ```ignore
//! use streamlib::clap::ClapPluginHost;
//! use std::sync::{Arc, Mutex};
//!
//! // Load plugin (thread-safe version)
//! let host = ClapPluginHost::<Arc<Mutex<_>>>::load_by_name(
//!     "/path/to/reverb.clap",
//!     "Reverb",
//!     48000,
//!     2048
//! )?;
//!
//! // Activate
//! host.activate(48000, 2048)?;
//!
//! // Process audio
//! let output = host.process_audio(&input_frame)?;
//! ```

use clack_host::{
    prelude::*,
    host::HostInfo,
    bundle::PluginBundle,
    plugin::PluginInstance,
    process::StartedPluginAudioProcessor,
    events::{
        event_types::ParamValueEvent,
        io::EventBuffer,
    },
    utils::{ClapId, Cookie},
};

use clack_extensions::params::{PluginParams, ParamInfoBuffer};

use crate::core::{Result, StreamError, AudioFrame};
use crate::core::clap::{ParameterInfo, PluginInfo};
use super::buffer_conversion::{deinterleave_audio_frame, interleave_to_audio_frame};
use super::scanner::ClapScanner;

use parking_lot::Mutex as ParkingLotMutex;
use std::sync::Arc;
use std::path::Path;
use std::marker::PhantomData;

/// Shared state accessible from all threads
struct SharedState {
    /// Current parameter values (id -> native value, NOT normalized)
    parameters: std::collections::HashMap<u32, f64>,

    /// Parameter info cache
    parameter_info: Vec<ParameterInfo>,

    /// Sample rate
    sample_rate: u32,

    /// Maximum frames per process call
    max_frames: usize,
}

/// Host data passed to plugin (main thread context)
struct HostData {
    shared: Arc<ParkingLotMutex<SharedState>>,
}

impl HostData {
    fn new(shared: Arc<ParkingLotMutex<SharedState>>) -> Self {
        Self { shared }
    }
}

/// Shared host data accessible from all contexts
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
        // For now, we just log. In a full implementation, we'd set a flag
        // and handle restart on next process cycle
    }

    fn request_process(&self) {
        tracing::debug!("Plugin requested process");
        // This is called when plugin wants to be processed (e.g., tail processing)
    }

    fn request_callback(&self) {
        tracing::debug!("Plugin requested callback");
        // Queue callback for main thread
    }
}

/// CLAP plugin host
///
/// Provides reusable CLAP plugin hosting infrastructure. Can be used by any transformer
/// that needs CLAP plugin integration.
///
/// # Type Parameters
///
/// Generic over synchronization primitive (not exposed in type signature):
/// - Uses `Arc<Mutex<_>>` internally for thread safety
/// - Future: Could support `RefCell<_>` for single-threaded optimization
pub struct ClapPluginHost {
    /// Loaded plugin bundle
    bundle: PluginBundle,

    /// Plugin ID (stored as CString for PluginInstance::new)
    plugin_id: std::ffi::CString,

    /// Plugin metadata
    plugin_info: PluginInfo,

    /// Plugin instance (created on activate)
    instance: Arc<ParkingLotMutex<Option<PluginInstance<HostData>>>>,

    /// Audio processor (created after activation)
    audio_processor: Arc<ParkingLotMutex<Option<StartedPluginAudioProcessor<HostData>>>>,

    /// Shared state
    shared_state: Arc<ParkingLotMutex<SharedState>>,

    /// Activation state
    is_activated: bool,

    /// Audio configuration
    sample_rate: u32,
    buffer_size: usize,
}

// SAFETY: ClapPluginHost is Send despite PluginBundle containing raw pointers
// because all shared state is protected by Arc<Mutex<...>> which ensures thread safety.
unsafe impl Send for ClapPluginHost {}

impl ClapPluginHost {
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
    /// let host = ClapPluginHost::load_by_name(
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
        Self::load_internal(path, Some(plugin_name), sample_rate, buffer_size)
    }

    /// Load a plugin by index from a CLAP bundle
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the CLAP plugin bundle
    /// * `index` - Zero-based index of the plugin to load
    /// * `sample_rate` - Sample rate for audio processing
    /// * `buffer_size` - Buffer size for audio processing
    pub fn load_by_index<P: AsRef<Path>>(
        path: P,
        index: usize,
        sample_rate: u32,
        buffer_size: usize
    ) -> Result<Self> {
        Self::load_internal_with_filter(path, sample_rate, buffer_size, |mut descriptors| {
            descriptors.nth(index).ok_or_else(|| StreamError::Configuration(
                format!("Plugin index {} not found in bundle", index)
            ))
        })
    }

    /// Load the first plugin from a CLAP bundle
    ///
    /// For bundles with multiple plugins, use `load_by_name()` or `load_by_index()`.
    pub fn load<P: AsRef<Path>>(
        path: P,
        sample_rate: u32,
        buffer_size: usize
    ) -> Result<Self> {
        Self::load_internal(path, None, sample_rate, buffer_size)
    }

    /// Internal method to load a plugin with optional name filter
    fn load_internal<P: AsRef<Path>>(
        path: P,
        plugin_name: Option<&str>,
        sample_rate: u32,
        buffer_size: usize
    ) -> Result<Self> {
        if let Some(name) = plugin_name {
            let name = name.to_string();
            Self::load_internal_with_filter(path, sample_rate, buffer_size, |mut descriptors| {
                descriptors.find(|desc| {
                    desc.name()
                        .and_then(|n| n.to_str().ok())
                        .map(|n| n == name)
                        .unwrap_or(false)
                })
                .ok_or_else(|| StreamError::Configuration(
                    format!("Plugin '{}' not found in bundle", name)
                ))
            })
        } else {
            Self::load_internal_with_filter(path, sample_rate, buffer_size, |mut descriptors| {
                descriptors.next()
                    .ok_or_else(|| StreamError::Configuration(
                        "CLAP plugin bundle contains no plugins".into()
                    ))
            })
        }
    }

    /// Internal method to load a plugin using a filter function
    fn load_internal_with_filter<P, F>(
        path: P,
        sample_rate: u32,
        buffer_size: usize,
        filter: F
    ) -> Result<Self>
    where
        P: AsRef<Path>,
        F: for<'a> FnOnce(clack_host::factory::PluginDescriptorsIter<'a>) -> Result<clack_host::factory::PluginDescriptor<'a>>,
    {
        let path = path.as_ref();

        // Get the actual binary path within the bundle (on macOS, bundles are folders)
        let binary_path = ClapScanner::get_bundle_binary_path(path)?;

        // Load plugin bundle
        // SAFETY: Loading CLAP plugins is inherently unsafe as it loads dynamic libraries
        // We trust that the plugin path points to a valid CLAP plugin
        let bundle = unsafe {
            PluginBundle::load(&binary_path)
                .map_err(|e| StreamError::Configuration(
                    format!("Failed to load CLAP plugin from {:?}: {:?}", path, e)
                ))?
        };

        // Get plugin factory
        let factory = bundle.get_plugin_factory()
            .ok_or_else(|| StreamError::Configuration(
                "CLAP plugin has no plugin factory".into()
            ))?;

        // Find the plugin descriptor using the filter
        let descriptor = filter(factory.plugin_descriptors())?;

        // Get plugin ID for both metadata and instance creation
        let plugin_id = descriptor.id()
            .ok_or_else(|| StreamError::Configuration(
                "Plugin descriptor has no ID".into()
            ))?;

        // Convert to string for metadata
        let plugin_id_str = plugin_id.to_str()
            .ok()
            .ok_or_else(|| StreamError::Configuration("Invalid plugin ID".into()))?
            .to_string();

        // Convert to CString for PluginInstance::new()
        let plugin_id_cstring = std::ffi::CString::new(plugin_id_str.clone())
            .map_err(|e| StreamError::Configuration(
                format!("Failed to create CString from plugin ID: {}", e)
            ))?;

        // Create plugin info
        let plugin_info = PluginInfo {
            name: descriptor.name()
                .and_then(|s| s.to_str().ok())
                .unwrap_or("Unknown")
                .to_string(),
            vendor: descriptor.vendor()
                .and_then(|s| s.to_str().ok())
                .unwrap_or("Unknown")
                .to_string(),
            version: descriptor.version()
                .and_then(|s| s.to_str().ok())
                .unwrap_or("Unknown")
                .to_string(),
            format: "CLAP".to_string(),
            id: plugin_id_str,
            num_inputs: 2,  // Will be updated after activation
            num_outputs: 2, // Will be updated after activation
        };

        // Create shared state
        let shared_state = Arc::new(ParkingLotMutex::new(SharedState {
            parameters: std::collections::HashMap::new(),
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
        })
    }

    /// Get plugin metadata
    pub fn plugin_info(&self) -> &PluginInfo {
        &self.plugin_info
    }

    /// List all parameters
    pub fn list_parameters(&self) -> Vec<ParameterInfo> {
        let state = self.shared_state.lock();
        state.parameter_info.clone()
    }

    /// Get parameter value (in native units, not normalized)
    pub fn get_parameter(&self, id: u32) -> Result<f64> {
        let state = self.shared_state.lock();
        state.parameters.get(&id)
            .copied()
            .ok_or_else(|| StreamError::Configuration(
                format!("Parameter ID {} not found", id)
            ))
    }

    /// Set parameter value (in native units, not normalized)
    ///
    /// The value will be sent to the plugin during the next `process_audio()` call.
    pub fn set_parameter(&mut self, id: u32, value: f64) -> Result<()> {
        let mut state = self.shared_state.lock();
        state.parameters.insert(id, value);

        tracing::info!("Parameter {} set to {} (will be sent during next process)", id, value);

        Ok(())
    }

    /// Begin parameter edit transaction
    ///
    /// Note: Current version of clack-host doesn't expose these CLAP extension methods.
    /// This is a placeholder for future implementation.
    pub fn begin_edit(&mut self, id: u32) -> Result<()> {
        tracing::debug!("begin_edit({}) - placeholder (not yet implemented)", id);
        Ok(())
    }

    /// End parameter edit transaction
    ///
    /// Note: Current version of clack-host doesn't expose these CLAP extension methods.
    /// This is a placeholder for future implementation.
    pub fn end_edit(&mut self, id: u32) -> Result<()> {
        tracing::debug!("end_edit({}) - placeholder (not yet implemented)", id);
        Ok(())
    }

    /// Activate the plugin (idempotent)
    ///
    /// If already activated, this is a no-op.
    pub fn activate(&mut self, sample_rate: u32, max_frames: usize) -> Result<()> {
        if self.is_activated {
            tracing::debug!("Plugin already activated, skipping");
            return Ok(());
        }

        // Update shared state
        {
            let mut state = self.shared_state.lock();
            state.sample_rate = sample_rate;
            state.max_frames = max_frames;
        }

        // Create host info
        let host_info = HostInfo::new(
            "streamlib",
            "Tato Lab",
            "https://github.com/tatolab/streamlib",
            "0.1.0"
        ).map_err(|e| StreamError::Configuration(
            format!("Failed to create host info: {:?}", e)
        ))?;

        // Clone shared state for closures
        let shared_state_clone = Arc::clone(&self.shared_state);

        // Create plugin instance
        let mut instance = PluginInstance::<HostData>::new(
            |_| {
                // Create host shared data inside closure
                HostShared {
                    state: Arc::clone(&shared_state_clone),
                }
            },
            |_shared| (),  // Main thread handler factory
            &self.bundle,
            &self.plugin_id,
            &host_info,
        ).map_err(|e| StreamError::Configuration(
            format!("Failed to create plugin instance: {:?}", e)
        ))?;

        // Enumerate parameters from the plugin (before activation)
        {
            let mut main_thread_handle = instance.plugin_handle();
            let shared_handle = main_thread_handle.shared();

            if let Some(params_ext) = shared_handle.get_extension::<PluginParams>() {
                let param_count = params_ext.count(&mut main_thread_handle);

                tracing::info!("Plugin has {} parameters", param_count);

                let mut param_buffer = ParamInfoBuffer::new();
                let mut parameter_infos = Vec::new();

                for i in 0..param_count {
                    if let Some(param_info) = params_ext.get_info(&mut main_thread_handle, i, &mut param_buffer) {
                        let name = std::str::from_utf8(param_info.name)
                            .unwrap_or("Unknown")
                            .trim_end_matches('\0')
                            .to_string();

                        let value = params_ext.get_value(&mut main_thread_handle, param_info.id)
                            .unwrap_or(param_info.default_value);

                        let flags = param_info.flags;
                        let is_automatable = flags.contains(
                            clack_extensions::params::ParamInfoFlags::IS_AUTOMATABLE
                        );
                        let is_stepped = flags.contains(
                            clack_extensions::params::ParamInfoFlags::IS_STEPPED
                        );
                        let is_periodic = flags.contains(
                            clack_extensions::params::ParamInfoFlags::IS_PERIODIC
                        );
                        let is_hidden = flags.contains(
                            clack_extensions::params::ParamInfoFlags::IS_HIDDEN
                        );
                        let is_readonly = flags.contains(
                            clack_extensions::params::ParamInfoFlags::IS_READONLY
                        );
                        let is_bypass = flags.contains(
                            clack_extensions::params::ParamInfoFlags::IS_BYPASS
                        );

                        let mut display_buf = vec![std::mem::MaybeUninit::new(0u8); 256];
                        let display = params_ext.value_to_text(
                            &mut main_thread_handle,
                            param_info.id,
                            value,
                            &mut display_buf
                        ).ok()
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
                            i, name, param_info.id.get(), value,
                            param_info.min_value, param_info.max_value
                        );
                    }
                }

                self.shared_state.lock().parameter_info = parameter_infos;
            } else {
                tracing::warn!("Plugin does not support parameter extension");
            }
        }

        // Create audio configuration
        let audio_config = PluginAudioConfiguration {
            sample_rate: sample_rate as f64,
            min_frames_count: 1,
            max_frames_count: max_frames as u32,
        };

        // Activate plugin audio processor
        let activated = instance.activate(|_, _| (), audio_config)
            .map_err(|e| StreamError::Configuration(
                format!("Failed to activate plugin: {:?}", e)
            ))?;

        // Start processing
        let audio_processor = activated.start_processing()
            .map_err(|e| StreamError::Configuration(
                format!("Failed to start audio processing: {:?}", e)
            ))?;

        // Store instance and processor
        *self.instance.lock() = Some(instance);
        *self.audio_processor.lock() = Some(audio_processor);

        self.is_activated = true;
        self.sample_rate = sample_rate;
        self.buffer_size = max_frames;

        tracing::info!(
            "✅ Activated CLAP plugin '{}' at {}Hz, {} max frames",
            self.plugin_info.name,
            sample_rate,
            max_frames
        );

        Ok(())
    }

    /// Deactivate the plugin
    pub fn deactivate(&mut self) -> Result<()> {
        if !self.is_activated {
            return Ok(());
        }

        // Drop audio processor (stops processing)
        *self.audio_processor.lock() = None;

        // Drop plugin instance (deactivates)
        *self.instance.lock() = None;

        self.is_activated = false;

        tracing::info!("✅ Deactivated CLAP plugin '{}'", self.plugin_info.name);

        Ok(())
    }

    /// Process audio through the plugin (convenience API)
    ///
    /// Handles buffer conversion internally. For more control, use `process_audio_channels()`.
    pub fn process_audio(&mut self, input: &AudioFrame) -> Result<AudioFrame> {
        let channel_buffers = deinterleave_audio_frame(input);

        assert!(channel_buffers.len() >= 2, "CLAP plugins require at least stereo input");

        let (left_out, right_out) = self.process_audio_channels(&channel_buffers[0], &channel_buffers[1])?;
        Ok(interleave_to_audio_frame(&[left_out, right_out], input.timestamp_ns, input.frame_number))
    }

    /// Process audio through the plugin (low-level API)
    ///
    /// Processes separate channel buffers directly without AudioFrame conversion.
    pub fn process_audio_channels(
        &mut self,
        left_in: &[f32],
        right_in: &[f32],
    ) -> Result<(Vec<f32>, Vec<f32>)> {
        if !self.is_activated {
            return Err(StreamError::Configuration(
                "Plugin not activated - call activate() first".into()
            ));
        }

        let num_samples = left_in.len();

        // Create output buffers (same size as input)
        let left_out = vec![0.0f32; num_samples];
        let right_out = vec![0.0f32; num_samples];

        // Get audio processor
        let mut processor_guard = self.audio_processor.lock();
        let processor = processor_guard.as_mut()
            .ok_or_else(|| StreamError::Configuration(
                "Audio processor not started".into()
            ))?;

        // Create CLAP audio port buffers
        let mut all_input_channels = vec![left_in.to_vec(), right_in.to_vec()];
        let mut all_output_channels = vec![left_out, right_out];

        let mut input_ports_base = AudioPorts::with_capacity(2, 1);
        let mut output_ports_base = AudioPorts::with_capacity(2, 1);

        let input_ports = input_ports_base.with_input_buffers(
            std::iter::once(AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_input_only(
                    all_input_channels.iter_mut().map(|buf| InputChannel {
                        buffer: &mut buf[..],
                        is_constant: false,
                    })
                ),
            })
        );

        let mut output_ports = output_ports_base.with_output_buffers(
            std::iter::once(AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_output_only(
                    all_output_channels.iter_mut().map(|buf| &mut buf[..])
                ),
            })
        );

        // Create parameter change events
        let parameters = self.shared_state.lock().parameters.clone();

        let mut event_buffer = EventBuffer::with_capacity(parameters.len());

        for (param_id, value) in parameters.iter() {
            tracing::debug!("Adding param event: id={}, value={:.4}", param_id, value);
            let event = ParamValueEvent::new(
                0,  // time: apply at start of buffer
                ClapId::new(*param_id),
                Pckn::match_all(),
                *value,
                Cookie::empty(),
            );
            event_buffer.push(&event);
        }

        let input_events = event_buffer.as_input();

        // Process audio through plugin
        processor.process(
            &input_ports,
            &mut output_ports,
            &input_events,
            &mut OutputEvents::void(),
            None,  // steady_counter
            None,  // transport
        ).map_err(|e| StreamError::Runtime(
            format!("Plugin processing failed: {:?}", e)
        ))?;

        Ok((all_output_channels[0].clone(), all_output_channels[1].clone()))
    }
}
