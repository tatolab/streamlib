// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::processor_audio_converter::{ProcessorAudioConverter, ProcessorAudioConverterTargetFormat};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Stream, StreamConfig};
use rtrb::{Producer, RingBuffer};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use crate::_generated_::AudioFrame;
use streamlib_plugin_sdk::sdk::error::{Result, Error};
use streamlib_plugin_sdk::sdk::context::RuntimeContextFullAccess;

/// Wrapper for InputMailboxes pointer that is Send.
struct SendableInputsPtr(*const streamlib_plugin_sdk::sdk::iceoryx2::InputMailboxes);

// SAFETY: InputMailboxes is Send, and we control the lifetime
unsafe impl Send for SendableInputsPtr {}

impl SendableInputsPtr {
    /// SAFETY: Caller must ensure the pointed-to data is still valid.
    unsafe fn get(&self) -> &streamlib_plugin_sdk::sdk::iceoryx2::InputMailboxes {
        &*self.0
    }
}

/// Wrapper for ProcessorAudioConverter pointer that is Send.
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

#[derive(Debug, Clone)]
pub struct AppleAudioDevice {
    pub id: usize,
    pub name: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub is_default: bool,
}

#[streamlib_plugin_sdk::sdk::processor(
    "@tatolab/audio/AudioOutput@1.0.0",
    execution = manual,
    scheduling = realtime,
    config = crate::_generated_::AudioOutputConfig,
    input("audio", "@tatolab/core/AudioFrame@1.0.0", read_mode = "read_next_in_order", buffer_size = 32),
)]
pub struct AppleAudioOutputProcessor {
    device_id: Option<usize>,
    device_name: String,
    device_info: Option<AppleAudioDevice>,
    stream: Option<Stream>,
    stream_setup_done: bool,
    sample_rate: u32,
    channels: u32,
    buffer_size: usize,
    frame_producer: Arc<Mutex<Option<Producer<AudioFrame>>>>,
    polling_thread: Option<thread::JoinHandle<()>>,
    stop_polling: Arc<AtomicBool>,
    audio: Option<ProcessorAudioConverter>,
}

impl streamlib_plugin_sdk::sdk::processors::ManualProcessor for AppleAudioOutputProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.device_id = self
            .config
            .device_id
            .as_ref()
            .and_then(|s| s.parse::<usize>().ok());
        self.audio = Some(ProcessorAudioConverter::new());
        tracing::info!(
            "AudioOutput: start() called (Pull mode - will query device for native config)"
        );
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.stop_polling.store(true, Ordering::SeqCst);

        if let Some(handle) = self.polling_thread.take() {
            let _ = handle.join();
        }

        self.stream = None;
        tracing::info!("AudioOutput {}: Stopped", self.device_name);
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        if self.stream_setup_done {
            return Ok(());
        }

        tracing::info!(
            "AudioOutput: process() called - setting up stream now that connections are wired"
        );

        let host = cpal::default_host();
        let device = if let Some(id) = self.device_id {
            let devices: Vec<_> = host
                .output_devices()
                .map_err(|e| {
                    Error::Configuration(format!("Failed to enumerate audio devices: {}", e))
                })?
                .collect();
            devices
                .get(id)
                .ok_or_else(|| {
                    Error::Configuration(format!("Audio device {} not found", id))
                })?
                .clone()
        } else {
            host.default_output_device().ok_or_else(|| {
                Error::Configuration("No default audio output device".into())
            })?
        };

        let device_config = device.default_output_config().map_err(|e| {
            Error::Configuration(format!("Failed to get audio config: {}", e))
        })?;

        let device_sample_rate = device_config.sample_rate().0;
        let device_channels = device_config.channels() as u32;

        let device_buffer_size = match device_config.buffer_size() {
            cpal::SupportedBufferSize::Range { min: _, max } => *max as usize,
            cpal::SupportedBufferSize::Unknown => 512,
        };

        tracing::info!(
            "AudioOutput: Queried device config - {}Hz, {} channels, {} buffer size",
            device_sample_rate,
            device_channels,
            device_buffer_size
        );

        let (producer, consumer) = RingBuffer::<AudioFrame>::new(256);

        let consumer = Arc::new(Mutex::new(consumer));
        let consumer_for_callback = Arc::clone(&consumer);

        tracing::info!("AudioOutput: Setting up adaptive audio output with cpal");

        let mut sample_buffer: Vec<f32> = Vec::new();

        let stream_config = StreamConfig {
            channels: device_channels as u16,
            sample_rate: cpal::SampleRate(device_sample_rate),
            buffer_size: cpal::BufferSize::Fixed(device_buffer_size as u32),
        };

        let stream = device
            .build_output_stream(
                &stream_config,
                move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                    let mut consumer_guard = consumer_for_callback.lock().unwrap();

                    while sample_buffer.len() < data.len() {
                        if let Ok(audio_frame) = consumer_guard.pop() {
                            sample_buffer.extend_from_slice(&audio_frame.samples);
                        } else {
                            break;
                        }
                    }

                    if sample_buffer.len() >= data.len() {
                        data.copy_from_slice(&sample_buffer[..data.len()]);
                        sample_buffer.drain(..data.len());
                    } else if !sample_buffer.is_empty() {
                        let copy_len = sample_buffer.len();
                        data[..copy_len].copy_from_slice(&sample_buffer);
                        data[copy_len..].fill(0.0);
                        sample_buffer.clear();
                    } else {
                        data.fill(0.0);
                    }
                },
                |err| {
                    tracing::error!("Audio output stream error: {}", err);
                },
                None,
            )
            .map_err(|e| {
                Error::Configuration(format!("Failed to build audio stream: {}", e))
            })?;

        tracing::info!("AudioOutput: Starting cpal stream playback");
        stream
            .play()
            .map_err(|e| Error::Configuration(format!("Failed to start stream: {}", e)))?;

        tracing::info!("AudioOutput: cpal stream.play() succeeded");

        {
            let mut producer_guard = self.frame_producer.lock().unwrap();
            *producer_guard = Some(producer);
        }

        let stop_flag = Arc::clone(&self.stop_polling);
        stop_flag.store(false, Ordering::SeqCst);

        // SAFETY for both raw pointers:
        // 1. The polling thread is stopped in teardown() before self is dropped
        // 2. Only the polling thread accesses these after start() returns
        // 3. In Manual mode, no other code touches self between start() and teardown()
        let audio = self.audio.as_mut().ok_or_else(|| {
            Error::Configuration(
                "audio converter not initialized — setup() must run before start()".into(),
            )
        })?;
        let inputs_ptr = SendableInputsPtr(&self.inputs as *const _);
        let audio_ptr = SendableAudioConverterPtr(audio as *mut _);
        let producer_clone = Arc::clone(&self.frame_producer);
        let stop_clone = Arc::clone(&stop_flag);
        let target_sample_rate = device_sample_rate;
        let target_channels = device_channels as u8;

        let polling_thread = thread::spawn(move || {
            tracing::info!("[AudioOutput] Polling thread started");

            let target = ProcessorAudioConverterTargetFormat {
                sample_rate: Some(target_sample_rate),
                channels: Some(target_channels),
                buffer_size: None,
            };

            while !stop_clone.load(Ordering::SeqCst) {
                let inputs = unsafe { inputs_ptr.get() };

                if inputs.has_data("audio") {
                    if let Ok(frame) = inputs.read::<AudioFrame>("audio") {
                        let audio = unsafe { audio_ptr.get_mut() };

                        match audio.convert(&frame, &target) {
                            Ok(converted_frames) => {
                                let mut producer_guard = producer_clone.lock().unwrap();
                                if let Some(ref mut producer) = *producer_guard {
                                    for converted in converted_frames {
                                        if producer.push(converted).is_err() {
                                            tracing::warn!(
                                                "[AudioOutput] Ring buffer full, dropping frame"
                                            );
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::error!("[AudioOutput] Audio conversion failed: {}", e);
                            }
                        }
                    }
                } else {
                    thread::sleep(std::time::Duration::from_micros(500));
                }
            }

            tracing::info!("[AudioOutput] Polling thread stopped");
        });

        self.polling_thread = Some(polling_thread);

        let device_name = device
            .name()
            .unwrap_or_else(|_| "Unknown Device".to_string());

        self.stream = Some(stream);
        self.device_name = device_name.clone();
        self.device_info = Some(AppleAudioDevice {
            id: self.device_id.unwrap_or(0),
            name: device_name,
            sample_rate: device_sample_rate,
            channels: device_channels,
            is_default: self.device_id.is_none(),
        });
        self.sample_rate = device_sample_rate;
        self.channels = device_channels;
        self.buffer_size = device_buffer_size;
        self.stream_setup_done = true;

        tracing::info!(
            "AudioOutput {}: Stream setup complete ({}Hz, {} channels, {} buffer size, Pull mode)",
            self.device_name,
            self.sample_rate,
            self.channels,
            self.buffer_size
        );

        Ok(())
    }
}
