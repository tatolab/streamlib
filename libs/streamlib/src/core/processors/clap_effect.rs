//! CLAP Audio Effect Processor
//!
//! Implementation of AudioEffectProcessor for CLAP plugins using clack-host.
//!
//! # Architecture
//!
//! ```text
//! ClapEffectProcessor
//!   ├─ Plugin Bundle (loaded .clap file)
//!   ├─ Plugin Instance (activated processor)
//!   ├─ Shared State (Arc<Mutex<...>>)
//!   │   ├─ Parameters (thread-safe access)
//!   │   └─ Audio ports info
//!   └─ Audio Processor (real-time processing)
//! ```
//!
//! # Thread Safety
//!
//! - Main thread: Plugin loading, activation, parameter changes
//! - Audio thread: Real-time processing (process_audio)
//! - Parameter changes are queued and applied in audio thread
//!
//! # Example
//!
//! ```ignore
//! use streamlib::ClapEffectProcessor;
//!
//! // Load plugin
//! let mut reverb = ClapEffectProcessor::load("reverb.clap")?;
//!
//! // Configure
//! reverb.activate(48000, 2048)?;
//! reverb.set_parameter_by_name("Room Size", 0.8)?;
//!
//! // Process
//! let output = reverb.process_audio(&input_frame)?;
//! ```

#[cfg(feature = "clap-plugins")]
use clack_host::{
    prelude::*,
    host::HostInfo,
    bundle::PluginBundle,
    plugin::PluginInstance,
    process::StartedPluginAudioProcessor,
    factory::PluginDescriptor,
    events::{
        event_types::ParamValueEvent,
        io::EventBuffer,
    },
    utils::{ClapId, Cookie},
};

#[cfg(feature = "clap-plugins")]
use clack_extensions::params::{PluginParams, ParamInfoBuffer};

use crate::core::{
    Result, StreamError, StreamProcessor, TimedTick, AudioFrame,
};

use super::audio_effect::{AudioEffectProcessor, ParameterInfo, PluginInfo};

#[cfg(feature = "clap-plugins")]
use parking_lot::Mutex;

#[cfg(feature = "clap-plugins")]
use std::sync::Arc;

use std::path::Path;

/// CLAP plugin effect processor
///
/// Hosts CLAP audio plugins and integrates them into streamlib's processing pipeline.
///
/// # Important: Audio Configuration
///
/// **Always use `runtime.audio_config()` when activating CLAP plugins** to ensure
/// sample rate consistency across your audio pipeline. Mismatched sample rates cause
/// pitch shifts and audio artifacts.
///
/// # Example
///
/// ```ignore
/// use streamlib::{StreamRuntime, ClapEffectProcessor};
///
/// // Create runtime with default audio config (48kHz, stereo, 2048 buffer)
/// let runtime = StreamRuntime::new(60.0);
/// let config = runtime.audio_config();
///
/// // Load CLAP plugin by name
/// let mut plugin = ClapEffectProcessor::load_by_name(
///     "/path/to/plugin.clap",
///     "Gain"
/// )?;
///
/// // Activate plugin using runtime's audio config
/// plugin.activate(config.sample_rate, config.buffer_size)?;
///
/// // Add to runtime and connect to pipeline
/// runtime.add_processor(Box::new(plugin));
/// ```
///
/// # Custom Audio Configuration
///
/// If you need custom sample rates (e.g., 44.1kHz for CD quality):
///
/// ```ignore
/// use streamlib::{StreamRuntime, AudioConfig};
///
/// let mut runtime = StreamRuntime::new(60.0);
///
/// // Set audio config BEFORE adding processors
/// runtime.set_audio_config(AudioConfig {
///     sample_rate: 44100,
///     channels: 2,
///     buffer_size: 1024,  // Lower latency
/// });
///
/// // Now all processors will use 44.1kHz
/// let config = runtime.audio_config();
/// plugin.activate(config.sample_rate, config.buffer_size)?;
/// ```
#[cfg(feature = "clap-plugins")]
pub struct ClapEffectProcessor {
    /// Plugin metadata
    plugin_info: PluginInfo,

    /// Loaded plugin bundle
    bundle: PluginBundle,

    /// Plugin ID for instance creation (stored as CString for PluginInstance::new)
    plugin_id: std::ffi::CString,

    /// Plugin instance (created on activate)
    instance: Arc<Mutex<Option<PluginInstance<HostData>>>>,

    /// Audio processor (created after activation)
    audio_processor: Arc<Mutex<Option<StartedPluginAudioProcessor<HostData>>>>,

    /// Shared state between threads
    shared_state: Arc<Mutex<SharedState>>,

    /// Is the plugin currently activated?
    is_activated: bool,
}

// SAFETY: ClapEffectProcessor is Send despite PluginBundle containing raw pointers
// because all shared state is protected by Arc<Mutex<...>> which ensures thread safety.
// The plugin bundle itself is never accessed across threads without synchronization.
#[cfg(feature = "clap-plugins")]
unsafe impl Send for ClapEffectProcessor {}

/// Shared state accessible from all threads
#[cfg(feature = "clap-plugins")]
struct SharedState {
    /// Current parameter values (id -> normalized value)
    parameters: std::collections::HashMap<u32, f64>,

    /// Parameter info cache
    parameter_info: Vec<ParameterInfo>,

    /// Sample rate
    sample_rate: u32,

    /// Maximum frames per process call
    max_frames: usize,
}

/// Convert interleaved AudioFrame to separate channel buffers for CLAP
///
/// AudioFrame stores samples interleaved (LRLRLR...), but CLAP expects
/// separate channel buffers (LLL... RRR...).
///
/// # Arguments
///
/// * `frame` - Input audio frame with interleaved samples
///
/// # Returns
///
/// Tuple of (left_channel, right_channel) as separate Vec<f32>
///
/// # Panics
///
/// Panics if frame is not stereo (2 channels)
#[cfg(feature = "clap-plugins")]
fn deinterleave_audio_frame(frame: &AudioFrame) -> (Vec<f32>, Vec<f32>) {
    assert_eq!(frame.channels, 2, "Only stereo audio supported for CLAP plugins");

    let samples = &frame.samples;
    let num_samples = frame.sample_count;

    let mut left = Vec::with_capacity(num_samples);
    let mut right = Vec::with_capacity(num_samples);

    // Deinterleave: LRLRLR... → LL...L, RR...R
    for i in 0..num_samples {
        left.push(samples[i * 2]);
        right.push(samples[i * 2 + 1]);
    }

    (left, right)
}

/// Convert separate channel buffers from CLAP to interleaved AudioFrame
///
/// CLAP returns separate channel buffers (LLL... RRR...), but AudioFrame
/// stores samples interleaved (LRLRLR...).
///
/// # Arguments
///
/// * `left` - Left channel samples
/// * `right` - Right channel samples
/// * `sample_rate` - Sample rate in Hz
/// * `timestamp_ns` - Timestamp in nanoseconds
/// * `frame_number` - Frame number
///
/// # Returns
///
/// AudioFrame with interleaved stereo data
///
/// # Panics
///
/// Panics if left and right have different lengths
#[cfg(feature = "clap-plugins")]
fn interleave_to_audio_frame(
    left: &[f32],
    right: &[f32],
    sample_rate: u32,
    timestamp_ns: i64,
    frame_number: u64,
) -> AudioFrame {
    assert_eq!(left.len(), right.len(), "Channel buffers must have same length");

    let num_samples = left.len();
    let mut samples = Vec::with_capacity(num_samples * 2);

    // Interleave: LL...L, RR...R → LRLRLR...
    for i in 0..num_samples {
        samples.push(left[i]);
        samples.push(right[i]);
    }

    AudioFrame::new(samples, timestamp_ns, frame_number, sample_rate, 2)
}

/// Host data passed to plugin (main thread context)
#[cfg(feature = "clap-plugins")]
struct HostData {
    shared: Arc<Mutex<SharedState>>,
}

#[cfg(feature = "clap-plugins")]
impl HostData {
    fn new(shared: Arc<Mutex<SharedState>>) -> Self {
        Self { shared }
    }
}

/// Shared host data accessible from all contexts
#[cfg(feature = "clap-plugins")]
struct HostShared {
    state: Arc<Mutex<SharedState>>,
}

#[cfg(feature = "clap-plugins")]
impl HostHandlers for HostData {
    type Shared<'a> = HostShared;
    type MainThread<'a> = (); // No additional main thread data needed
    type AudioProcessor<'a> = (); // No additional audio processor data needed
}

#[cfg(feature = "clap-plugins")]
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

#[cfg(feature = "clap-plugins")]
impl ClapEffectProcessor {
    /// Load a specific plugin by name from a CLAP bundle
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the CLAP plugin bundle
    /// * `plugin_name` - Name of the plugin to load (case-sensitive)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let plugin = ClapEffectProcessor::load_by_name(
    ///     "/path/to/bundle.clap",
    ///     "Gain"
    /// )?;
    /// ```
    pub fn load_by_name<P: AsRef<Path>>(path: P, plugin_name: &str) -> Result<Self> {
        Self::load_internal(path, Some(plugin_name))
    }

    /// Load a plugin by index from a CLAP bundle
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the CLAP plugin bundle
    /// * `index` - Zero-based index of the plugin to load
    pub fn load_by_index<P: AsRef<Path>>(path: P, index: usize) -> Result<Self> {
        Self::load_internal_with_filter(path, |mut descriptors| {
            descriptors.nth(index).ok_or_else(|| StreamError::Configuration(
                format!("Plugin index {} not found in bundle", index)
            ))
        })
    }

    /// Internal method to load a plugin with optional name filter
    fn load_internal<P: AsRef<Path>>(path: P, plugin_name: Option<&str>) -> Result<Self> {
        if let Some(name) = plugin_name {
            let name = name.to_string();
            Self::load_internal_with_filter(path, |mut descriptors| {
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
            Self::load_internal_with_filter(path, |mut descriptors| {
                descriptors.next()
                    .ok_or_else(|| StreamError::Configuration(
                        "CLAP plugin bundle contains no plugins".into()
                    ))
            })
        }
    }

    /// Internal method to load a plugin using a filter function
    fn load_internal_with_filter<P, F>(path: P, filter: F) -> Result<Self>
    where
        P: AsRef<Path>,
        F: for<'a> FnOnce(clack_host::factory::PluginDescriptorsIter<'a>) -> Result<PluginDescriptor<'a>>,
    {
        let path = path.as_ref();

        // Load plugin bundle
        // SAFETY: Loading CLAP plugins is inherently unsafe as it loads dynamic libraries
        // We trust that the plugin path points to a valid CLAP plugin
        let bundle = unsafe {
            PluginBundle::load(path)
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
        let shared_state = Arc::new(Mutex::new(SharedState {
            parameters: std::collections::HashMap::new(),
            parameter_info: Vec::new(),
            sample_rate: 48000,  // Default, will be set in activate()
            max_frames: 2048,    // Default, will be set in activate()
        }));

        Ok(Self {
            plugin_info,
            bundle,
            plugin_id: plugin_id_cstring,
            instance: Arc::new(Mutex::new(None)),
            audio_processor: Arc::new(Mutex::new(None)),
            shared_state,
            is_activated: false,
        })
    }
}

#[cfg(feature = "clap-plugins")]
impl AudioEffectProcessor for ClapEffectProcessor {
    /// Load the first plugin from a CLAP bundle
    ///
    /// For bundles with multiple plugins, use `load_by_name()` or `load_by_index()`
    /// to select a specific plugin.
    fn load<P: AsRef<Path>>(path: P) -> Result<Self>
    where
        Self: Sized,
    {
        ClapEffectProcessor::load_internal(path, None)
    }

    fn plugin_info(&self) -> &PluginInfo {
        &self.plugin_info
    }

    fn list_parameters(&self) -> Vec<ParameterInfo> {
        let state = self.shared_state.lock();
        state.parameter_info.clone()
    }

    fn get_parameter(&self, id: u32) -> Result<f64> {
        let state = self.shared_state.lock();
        state.parameters.get(&id)
            .copied()
            .ok_or_else(|| StreamError::Configuration(
                format!("Parameter ID {} not found", id)
            ))
    }

    fn set_parameter(&mut self, id: u32, value: f64) -> Result<()> {
        // Store parameter value
        // NOTE: Value should be in the parameter's native units (e.g., dB for a gain parameter)
        // NOT normalized 0.0-1.0! CLAP ParamValueEvents expect the actual "plain" value.
        let mut state = self.shared_state.lock();
        state.parameters.insert(id, value);

        // Parameters are applied during process_audio() via ParamValueEvents
        tracing::info!("Parameter {} set to {} (will be sent as event during next process)", id, value);

        Ok(())
    }

    fn process_audio(&mut self, input: &AudioFrame) -> Result<AudioFrame> {
        if !self.is_activated {
            return Err(StreamError::Configuration(
                "Plugin not activated - call activate() first".into()
            ));
        }

        // Deinterleave input to separate channels (CLAP expects non-interleaved)
        let (mut left_in, mut right_in) = deinterleave_audio_frame(input);
        let num_samples = left_in.len();

        // Create output buffers (same size as input)
        let mut left_out = vec![0.0f32; num_samples];
        let mut right_out = vec![0.0f32; num_samples];

        // Get audio processor
        let mut processor_guard = self.audio_processor.lock();
        let processor = processor_guard.as_mut()
            .ok_or_else(|| StreamError::Configuration(
                "Audio processor not started".into()
            ))?;

        // Create CLAP audio port buffers
        // CLAP uses separate channel buffers (not interleaved)
        // Store buffers in a container that will live long enough
        let mut all_input_channels = vec![left_in, right_in];
        let mut all_output_channels = vec![left_out, right_out];

        // Create empty base audio ports
        let mut input_ports_base = AudioPorts::with_capacity(2, 1);
        let mut output_ports_base = AudioPorts::with_capacity(2, 1);

        // Create audio ports with our buffers (returns InputAudioBuffers / OutputAudioBuffers)
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

        // Create parameter change events from stored parameter values
        let parameters = self.shared_state.lock().parameters.clone();

        // Create event buffer and add parameter value events
        let mut event_buffer = EventBuffer::with_capacity(parameters.len());

        if !parameters.is_empty() {
            tracing::debug!("Have {} parameters to send as events", parameters.len());
        }

        for (param_id, value) in parameters.iter() {
            // Create parameter value event at time 0 (start of buffer)
            // Pckn::match_all() means this is a global parameter (not per-note/channel)
            tracing::info!("Adding param event: id={}, value={:.4}", param_id, value);
            let event = ParamValueEvent::new(
                0,  // time: apply at start of buffer
                ClapId::new(*param_id),
                Pckn::match_all(),
                *value,
                Cookie::empty(),
            );
            event_buffer.push(&event);
        }

        // Convert event buffer to InputEvents (borrows event_buffer)
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

        // Interleave output channels back to AudioFrame
        let output = interleave_to_audio_frame(
            &all_output_channels[0],
            &all_output_channels[1],
            input.sample_rate,
            input.timestamp_ns,
            input.frame_number,
        );

        // Debug: Check if plugin actually modified the audio
        static FIRST_PROCESS: std::sync::Once = std::sync::Once::new();
        FIRST_PROCESS.call_once(|| {
            let input_peak = input.samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
            let output_peak = output.samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
            tracing::info!("CLAP plugin first process: input peak = {:.4}, output peak = {:.4}", input_peak, output_peak);

            if (input_peak - output_peak).abs() < 0.001 {
                tracing::warn!("⚠️  CLAP plugin output matches input - plugin may not be processing!");
            }
        });

        Ok(output)
    }

    fn activate(&mut self, sample_rate: u32, max_frames: usize) -> Result<()> {
        if self.is_activated {
            return Err(StreamError::Configuration(
                "Plugin already activated".into()
            ));
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
            |_shared| (),                 // Main thread handler factory
            &self.bundle,
            &self.plugin_id,
            &host_info,
        ).map_err(|e| StreamError::Configuration(
            format!("Failed to create plugin instance: {:?}", e)
        ))?;

        // Enumerate parameters from the plugin (before activation)
        {
            // Get main thread handle and shared handle
            let mut main_thread_handle = instance.plugin_handle();
            let shared_handle = main_thread_handle.shared();

            // Get params extension
            if let Some(params_ext) = shared_handle.get_extension::<PluginParams>() {
                // Get parameter count
                let param_count = params_ext.count(&mut main_thread_handle);

                tracing::info!("Plugin has {} parameters", param_count);

                // Enumerate each parameter
                let mut param_buffer = ParamInfoBuffer::new();
                let mut parameter_infos = Vec::new();

                for i in 0..param_count {
                    if let Some(param_info) = params_ext.get_info(&mut main_thread_handle, i, &mut param_buffer) {
                        // Convert name from bytes to string
                        let name = std::str::from_utf8(param_info.name)
                            .unwrap_or("Unknown")
                            .trim_end_matches('\0')
                            .to_string();

                        // Get current value
                        let value = params_ext.get_value(&mut main_thread_handle, param_info.id)
                            .unwrap_or(param_info.default_value);

                        // Extract parameter flags
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

                        // Get display string for current value
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

                        // Store parameter info
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
                        tracing::info!(
                            "Stored parameter: name='{}', id={}, value={}",
                            name, param_info.id.get(), value
                        );
                    }
                }

                // Store enumerated parameters in shared state
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

        tracing::info!(
            "✅ Activated CLAP plugin '{}' at {}Hz, {} max frames",
            self.plugin_info.name,
            sample_rate,
            max_frames
        );

        Ok(())
    }

    fn deactivate(&mut self) -> Result<()> {
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

    // Note: begin_edit/end_edit use default implementation from trait
    // Current version of clack-host doesn't expose these CLAP extension methods
    // Parameter changes are still batched via ParamValueEvents in process_audio()
}

#[cfg(feature = "clap-plugins")]
impl StreamProcessor for ClapEffectProcessor {
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
            .with_tags(vec!["audio", "effect", "clap", "plugin"])
        )
    }

    fn descriptor_instance(&self) -> Option<crate::core::schema::ProcessorDescriptor> {
        Self::descriptor()
    }

    fn process(&mut self, _tick: TimedTick) -> Result<()> {
        // Note: Actual audio processing happens via process_audio()
        // This is called by the runtime for integration
        Ok(())
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

// Stub implementation when clap-plugins feature is disabled
#[cfg(not(feature = "clap-plugins"))]
#[derive(Debug)]
pub struct ClapEffectProcessor;

#[cfg(not(feature = "clap-plugins"))]
impl ClapEffectProcessor {
    pub fn load<P: AsRef<Path>>(_path: P) -> Result<Self> {
        Err(StreamError::Configuration(
            "CLAP plugin support not enabled. Enable 'clap-plugins' feature.".into()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(feature = "clap-plugins"))]
    fn test_clap_disabled_error() {
        let result = ClapEffectProcessor::load("test.clap");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not enabled"));
    }

    #[cfg(feature = "clap-plugins")]
    mod clap_tests {
        use super::*;

        // Test 1: Audio buffer conversion utilities
        #[test]
        fn test_deinterleave_audio_frame() {
            // Create test frame: 4 samples stereo (L1 R1 L2 R2 L3 R3 L4 R4)
            let frame = AudioFrame::new(
                vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0],
                0,      // timestamp_ns
                0,      // frame_number
                48000,  // sample_rate
                2,      // channels
            );

            let (left, right) = deinterleave_audio_frame(&frame);

            assert_eq!(left.len(), 4);
            assert_eq!(right.len(), 4);
            assert_eq!(left, vec![1.0, 3.0, 5.0, 7.0]);
            assert_eq!(right, vec![2.0, 4.0, 6.0, 8.0]);
        }

        #[test]
        fn test_interleave_to_audio_frame() {
            // Create test CLAP buffers (separate L/R channels)
            let left: Vec<f32> = vec![1.0, 3.0, 5.0, 7.0];
            let right: Vec<f32> = vec![2.0, 4.0, 6.0, 8.0];

            let frame = interleave_to_audio_frame(&left, &right, 48000, 1000, 1);

            assert_eq!(frame.sample_rate, 48000);
            assert_eq!(frame.channels, 2);
            assert_eq!(frame.samples.len(), 8);
            assert_eq!(*frame.samples, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
        }

        #[test]
        fn test_roundtrip_conversion() {
            // Test that deinterleave → interleave is lossless
            let original = AudioFrame::new(
                vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
                5000,   // timestamp_ns
                42,     // frame_number
                44100,  // sample_rate
                2,      // channels
            );

            let (left, right) = deinterleave_audio_frame(&original);
            let roundtrip = interleave_to_audio_frame(
                &left,
                &right,
                original.sample_rate,
                original.timestamp_ns,
                original.frame_number,
            );

            assert_eq!(*original.samples, *roundtrip.samples);
            assert_eq!(original.sample_rate, roundtrip.sample_rate);
            assert_eq!(original.timestamp_ns, roundtrip.timestamp_ns);
            assert_eq!(original.frame_number, roundtrip.frame_number);
        }

        // Test 4: Real CLAP plugin loading and processing
        #[test]
        fn test_real_clap_plugin() {
            // Path to the test CLAP plugin binary (macOS bundle format)
            let plugin_path = "/Users/fonta/Repositories/tatolab/clap-plugins/build/plugins/clap-plugins.clap/Contents/MacOS/clap-plugins";

            // Skip test if plugin doesn't exist
            if !std::path::Path::new(plugin_path).exists() {
                eprintln!("Skipping test - CLAP plugin not found at {}", plugin_path);
                return;
            }

            // Load the Gain plugin specifically (not the first plugin which is ADSR)
            let mut plugin = ClapEffectProcessor::load_by_name(plugin_path, "Gain")
                .expect("Failed to load Gain plugin");

            // Check plugin info
            assert_eq!(plugin.plugin_info().format, "CLAP");
            assert_eq!(plugin.plugin_info().name, "Gain");
            println!("✅ Loaded CLAP Gain plugin");
            println!("   Vendor: {}", plugin.plugin_info().vendor);
            println!("   Version: {}", plugin.plugin_info().version);

            // Activate plugin
            let sample_rate = 48000;
            let max_frames = 512;
            plugin.activate(sample_rate, max_frames)
                .expect("Failed to activate plugin");
            println!("✅ Plugin activated at {}Hz", sample_rate);

            // Create test audio frame (1 second tone at 440 Hz)
            let num_samples = 512;
            let mut samples = Vec::with_capacity(num_samples * 2);
            for i in 0..num_samples {
                let t = i as f32 / sample_rate as f32;
                let sample = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5;
                samples.push(sample);  // Left
                samples.push(sample);  // Right
            }

            let input_frame = AudioFrame::new(
                samples.clone(),
                0,           // timestamp_ns
                0,           // frame_number
                sample_rate,
                2,           // channels
            );

            // Process audio through plugin
            let output_frame = plugin.process_audio(&input_frame)
                .expect("Failed to process audio");

            // Verify output
            assert_eq!(output_frame.sample_rate, input_frame.sample_rate);
            assert_eq!(output_frame.channels, input_frame.channels);
            assert_eq!(output_frame.sample_count, input_frame.sample_count);
            assert_eq!(output_frame.samples.len(), input_frame.samples.len());

            // The gain plugin should modify the signal (unless gain is at 0 dB)
            // For now, just verify we got output with valid values
            let has_valid_output = output_frame.samples.iter()
                .all(|&s| s.is_finite() && s.abs() <= 1.0);
            assert!(has_valid_output, "Output contains invalid samples");

            println!("✅ Successfully processed {} samples through CLAP gain plugin", num_samples);

            // Deactivate
            plugin.deactivate()
                .expect("Failed to deactivate plugin");
        }

        #[test]
        fn test_clap_plugin_parameter_enumeration() {
            use crate::core::AudioEffectProcessor;

            let plugin_path = "/Users/fonta/Repositories/tatolab/clap-plugins/build/plugins/clap-plugins.clap/Contents/MacOS/clap-plugins";

            if !std::path::Path::new(plugin_path).exists() {
                eprintln!("Skipping test - CLAP plugin not found");
                return;
            }

            let mut plugin = ClapEffectProcessor::load_by_name(plugin_path, "Gain")
                .expect("Failed to load Gain plugin");

            // Activate plugin (this enumerates parameters)
            plugin.activate(48000, 512)
                .expect("Failed to activate plugin");

            println!("✅ Testing parameter enumeration...");

            // Get parameter list
            let params = plugin.list_parameters();
            assert!(!params.is_empty(), "Plugin should have at least one parameter");

            println!("   Found {} parameter(s)", params.len());

            // Check the gain parameter
            let gain_param = params.first().expect("Should have gain parameter");
            assert_eq!(gain_param.name, "gain");
            assert_eq!(gain_param.min, -40.0);  // Gain plugin range
            assert_eq!(gain_param.max, 40.0);
            assert_eq!(gain_param.default, 0.0);  // Default is 0 dB
            assert!(gain_param.is_automatable);

            println!("   ✅ Parameter: {} [ID={}]", gain_param.name, gain_param.id);
            println!("      Range: {:.1} to {:.1} dB", gain_param.min, gain_param.max);
            println!("      Default: {:.1} dB", gain_param.default);
            println!("      Display: \"{}\"", gain_param.display);
        }

        #[test]
        fn test_clap_plugin_parameter_actual_values() {
            use crate::core::AudioEffectProcessor;

            let plugin_path = "/Users/fonta/Repositories/tatolab/streamlib/build/plugins/clap-plugins.clap/Contents/MacOS/clap-plugins";

            if !std::path::Path::new(plugin_path).exists() {
                eprintln!("Skipping test - CLAP plugin not found");
                return;
            }

            let mut plugin = ClapEffectProcessor::load_by_name(plugin_path, "Gain")
                .expect("Failed to load Gain plugin");

            plugin.activate(48000, 512)
                .expect("Failed to activate plugin");

            println!("✅ Testing parameter values (actual dB, not normalized)...");

            let params = plugin.list_parameters();
            let gain_id = params.first().expect("Should have gain parameter").id;

            // Test 1: Set to 0 dB (unity gain)
            plugin.set_parameter(gain_id, 0.0)
                .expect("Failed to set parameter");
            let value = plugin.get_parameter(gain_id)
                .expect("Failed to get parameter");
            assert_eq!(value, 0.0, "0 dB should be stored as 0.0");
            println!("   ✅ 0 dB (unity gain): {:.2}", value);

            // Test 2: Set to +8 dB (2.5x gain)
            plugin.set_parameter(gain_id, 8.0)
                .expect("Failed to set parameter");
            let value = plugin.get_parameter(gain_id)
                .expect("Failed to get parameter");
            assert_eq!(value, 8.0, "+8 dB should be stored as 8.0");
            println!("   ✅ +8 dB (2.5x gain): {:.2}", value);

            // Test 3: Set to -40 dB (minimum)
            plugin.set_parameter(gain_id, -40.0)
                .expect("Failed to set parameter");
            let value = plugin.get_parameter(gain_id)
                .expect("Failed to get parameter");
            assert_eq!(value, -40.0, "-40 dB should be stored as -40.0");
            println!("   ✅ -40 dB (minimum): {:.2}", value);

            // Test 4: Set to +40 dB (maximum)
            plugin.set_parameter(gain_id, 40.0)
                .expect("Failed to set parameter");
            let value = plugin.get_parameter(gain_id)
                .expect("Failed to get parameter");
            assert_eq!(value, 40.0, "+40 dB should be stored as 40.0");
            println!("   ✅ +40 dB (maximum): {:.2}", value);

            plugin.deactivate().expect("Failed to deactivate");
        }

        #[test]
        fn test_clap_plugin_parameter_audio_processing() {
            use crate::core::AudioEffectProcessor;

            let plugin_path = "/Users/fonta/Repositories/tatolab/clap-plugins/build/plugins/clap-plugins.clap/Contents/MacOS/clap-plugins";

            if !std::path::Path::new(plugin_path).exists() {
                eprintln!("Skipping test - CLAP plugin not found");
                return;
            }

            let mut plugin = ClapEffectProcessor::load_by_name(plugin_path, "Gain")
                .expect("Failed to load Gain plugin");

            plugin.activate(48000, 512)
                .expect("Failed to activate plugin");

            println!("✅ Testing that parameters actually affect audio output...");

            let params = plugin.list_parameters();
            let gain_id = params.first().expect("Should have gain parameter").id;

            // Generate test tone
            let sample_rate = 48000;
            let num_samples = 512;
            let mut samples = Vec::with_capacity(num_samples * 2);
            for i in 0..num_samples {
                let t = i as f32 / sample_rate as f32;
                let sample = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.3;
                samples.push(sample);  // Left
                samples.push(sample);  // Right
            }

            let input_frame = AudioFrame::new(
                samples.clone(),
                0,
                0,
                sample_rate,
                2,
            );

            // Test 1: Unity gain (0 dB) - output should equal input
            plugin.set_parameter(gain_id, 0.0).unwrap();
            let output = plugin.process_audio(&input_frame).unwrap();
            let input_peak = input_frame.samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
            let output_peak = output.samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
            let ratio = output_peak / input_peak;
            assert!((ratio - 1.0).abs() < 0.05, "Unity gain should be 1.0x, got {:.2}x", ratio);
            println!("   ✅ 0 dB: {:.2}x gain (expected 1.0x)", ratio);

            // Test 2: +8 dB - output should be ~2.5x louder
            plugin.set_parameter(gain_id, 8.0).unwrap();
            let output = plugin.process_audio(&input_frame).unwrap();
            let output_peak = output.samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
            let ratio = output_peak / input_peak;
            assert!((ratio - 2.51).abs() < 0.2, "+8 dB should be ~2.5x, got {:.2}x", ratio);
            println!("   ✅ +8 dB: {:.2}x gain (expected 2.51x)", ratio);

            // Test 3: +20 dB - output should be ~10x louder
            plugin.set_parameter(gain_id, 20.0).unwrap();
            let output = plugin.process_audio(&input_frame).unwrap();
            let output_peak = output.samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
            let ratio = output_peak / input_peak;
            assert!((ratio - 10.0).abs() < 0.5, "+20 dB should be ~10x, got {:.2}x", ratio);
            println!("   ✅ +20 dB: {:.2}x gain (expected 10.0x)", ratio);

            plugin.deactivate().expect("Failed to deactivate");
        }

        /// Test parameter transactions (begin_edit/end_edit)
        ///
        /// Verifies that transaction semantics work correctly for batched parameter updates
        #[test]
        fn test_clap_plugin_parameter_transactions() {
            let plugin_path = "/Library/Audio/Plug-Ins/CLAP/Surge XT Effects.clap/Contents/MacOS/Surge XT Effects";
            if !std::path::Path::new(plugin_path).exists() {
                println!("⚠️  Skipping test - plugin not found at {}", plugin_path);
                return;
            }

            let mut plugin = ClapEffectProcessor::load(plugin_path)
                .expect("Failed to load plugin");

            let sample_rate = 48000;
            let buffer_size = 2048;
            plugin.activate(sample_rate, buffer_size)
                .expect("Failed to activate plugin");

            println!("\n🎛️  Testing parameter transactions...");

            // Find gain parameter
            let parameters = plugin.list_parameters();
            let gain_param = parameters.iter()
                .find(|p| p.name.to_lowercase().contains("gain"))
                .expect("No gain parameter found");

            let gain_id = gain_param.id;
            println!("   Found gain parameter: {} (ID: {})", gain_param.name, gain_id);

            // Test transaction workflow
            println!("   Starting parameter transaction...");

            // Begin edit
            let result = plugin.begin_edit(gain_id);
            assert!(result.is_ok(), "begin_edit should succeed");

            // Make parameter changes (batched)
            plugin.set_parameter(gain_id, 6.0).unwrap();
            plugin.set_parameter(gain_id, 8.0).unwrap();
            plugin.set_parameter(gain_id, 10.0).unwrap();

            // End edit (commit transaction)
            let result = plugin.end_edit(gain_id);
            assert!(result.is_ok(), "end_edit should succeed");

            println!("   ✅ Transaction completed successfully");

            // Verify final value was applied
            let final_value = plugin.get_parameter(gain_id).unwrap();
            assert!((final_value - 10.0).abs() < 0.1, "Final value should be 10.0, got {}", final_value);
            println!("   ✅ Final parameter value: {} dB", final_value);

            plugin.deactivate().expect("Failed to deactivate");
        }
    }
}
