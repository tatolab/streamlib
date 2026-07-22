// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::processor_audio_converter::{ProcessorAudioConverter, ProcessorAudioConverterTargetFormat};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::StreamConfig;
use rtrb::{Producer, RingBuffer};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::sync::mpsc;
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
        unsafe { &*self.0 }
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
        unsafe { &mut *self.0 }
    }
}

#[derive(Debug, Clone)]
pub struct LinuxAudioDevice {
    pub id: usize,
    pub name: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub is_default: bool,
}

#[streamlib_plugin_sdk::sdk::processor(
    "@tatolab/audio/AudioOutput",
    description = "Plays audio through speakers/headphones (CoreAudio on macOS, ALSA on Linux)",
    execution = manual,
    scheduling = realtime,
    config = crate::_generated_::AudioOutputConfig,
    input("audio", "@tatolab/core/AudioFrame", description = "Stereo audio frame to play through speakers"),
)]
pub struct LinuxAudioOutputProcessor {
    device_id: Option<usize>,
    device_name: String,
    device_info: Option<LinuxAudioDevice>,
    stream_setup_done: bool,
    sample_rate: u32,
    channels: u32,
    buffer_size: usize,
    frame_producer: Arc<Mutex<Option<Producer<AudioFrame>>>>,
    // A `cpal::Stream` is `!Send`, but the `#[processor]` macro requires the
    // processor to be `Send`. The stream is therefore built and held on the
    // output thread (below) — which also runs the input-poll loop — and never
    // stored on the processor. The thread drops the stream when `stop_polling`
    // is set and it exits.
    output_thread: Option<thread::JoinHandle<()>>,
    stop_polling: Arc<AtomicBool>,
    audio: Option<ProcessorAudioConverter>,
}

/// The device configuration the output thread resolved, reported back to the
/// processor for its `current_device` summary.
struct ResolvedOutputConfig {
    device_name: String,
    sample_rate: u32,
    channels: u32,
    buffer_size: usize,
}

impl streamlib_plugin_sdk::sdk::processors::ManualProcessor for LinuxAudioOutputProcessor::Processor {
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

        // The output thread owns the `cpal::Stream`; joining it drops the
        // stream on its own thread.
        if let Some(handle) = self.output_thread.take() {
            let _ = handle.join();
        }

        self.stream_setup_done = false;
        tracing::info!("AudioOutput {}: Stopped", self.device_name);
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        if self.stream_setup_done {
            return Ok(());
        }

        tracing::info!(
            "AudioOutput: start() called - setting up stream now that connections are wired"
        );

        let (producer, consumer) = RingBuffer::<AudioFrame>::new(256);
        {
            let mut producer_guard = self.frame_producer.lock().unwrap();
            *producer_guard = Some(producer);
        }

        let stop_flag = Arc::clone(&self.stop_polling);
        stop_flag.store(false, Ordering::SeqCst);

        // SAFETY for both raw pointers:
        // 1. The output thread is stopped in teardown() before self is dropped
        // 2. Only the output thread accesses these after start() returns
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
        let device_id = self.device_id;

        let (ready_sender, ready_receiver) = mpsc::channel::<Result<ResolvedOutputConfig>>();

        let output_thread = thread::Builder::new()
            .name("audio-output".to_string())
            .spawn(move || {
                // A `cpal::Stream` is `!Send`: build, play, and hold it entirely
                // on this thread; the input-poll loop below runs on the same
                // thread and the stream drops when the loop exits.
                let (resolved, stream) = match build_output_stream(device_id, consumer) {
                    Ok(built) => built,
                    Err(e) => {
                        let _ = ready_sender.send(Err(e));
                        return;
                    }
                };

                let target = ProcessorAudioConverterTargetFormat {
                    sample_rate: Some(resolved.sample_rate),
                    channels: Some(resolved.channels as u8),
                    buffer_size: None,
                };

                if ready_sender.send(Ok(resolved)).is_err() {
                    return;
                }

                tracing::info!("[AudioOutput] Polling loop started");
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
                                    tracing::error!(
                                        "[AudioOutput] Audio conversion failed: {}",
                                        e
                                    );
                                }
                            }
                        }
                    } else {
                        thread::sleep(std::time::Duration::from_micros(500));
                    }
                }

                drop(stream);
                tracing::info!("[AudioOutput] Polling loop stopped");
            })
            .map_err(|e| {
                Error::Configuration(format!("Failed to spawn audio output thread: {}", e))
            })?;

        let resolved = ready_receiver.recv().map_err(|_| {
            Error::Configuration("Audio output thread exited before reporting stream setup".into())
        })??;

        self.output_thread = Some(output_thread);
        self.device_name = resolved.device_name.clone();
        self.device_info = Some(LinuxAudioDevice {
            id: self.device_id.unwrap_or(0),
            name: resolved.device_name,
            sample_rate: resolved.sample_rate,
            channels: resolved.channels,
            is_default: self.device_id.is_none(),
        });
        self.sample_rate = resolved.sample_rate;
        self.channels = resolved.channels;
        self.buffer_size = resolved.buffer_size;
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

/// Select the output device, build the `cpal` output stream fed by `consumer`,
/// and start playback. Runs entirely on the output thread because a
/// `cpal::Stream` is `!Send`; the resolved device config is returned so the
/// processor can report it.
fn build_output_stream(
    device_id: Option<usize>,
    consumer: rtrb::Consumer<AudioFrame>,
) -> Result<(ResolvedOutputConfig, cpal::Stream)> {
    let host = cpal::default_host();
    let device = if let Some(id) = device_id {
        let devices: Vec<_> = host
            .output_devices()
            .map_err(|e| {
                Error::Configuration(format!("Failed to enumerate audio devices: {}", e))
            })?
            .collect();
        devices
            .get(id)
            .ok_or_else(|| Error::Configuration(format!("Audio device {} not found", id)))?
            .clone()
    } else {
        host.default_output_device()
            .ok_or_else(|| Error::Configuration("No default audio output device".into()))?
    };

    let device_config = device.default_output_config().map_err(|e| {
        Error::Configuration(format!("Failed to get audio config: {}", e))
    })?;

    let device_sample_rate = device_config.sample_rate().0;
    let device_channels = device_config.channels() as u32;

    // 512 samples (~10ms at 48kHz) balances latency and reliability.
    let device_buffer_size = match device_config.buffer_size() {
        cpal::SupportedBufferSize::Range { min, max } => {
            let preferred = 512u32;
            preferred.clamp(*min, *max) as usize
        }
        cpal::SupportedBufferSize::Unknown => 512,
    };

    tracing::info!(
        "AudioOutput: Queried device config - {}Hz, {} channels, {} buffer size (ALSA backend)",
        device_sample_rate,
        device_channels,
        device_buffer_size
    );

    let consumer = Arc::new(Mutex::new(consumer));
    let consumer_for_callback = Arc::clone(&consumer);
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
        .map_err(|e| Error::Configuration(format!("Failed to build audio stream: {}", e)))?;

    tracing::info!("AudioOutput: Starting cpal stream playback");
    stream
        .play()
        .map_err(|e| Error::Configuration(format!("Failed to start stream: {}", e)))?;

    tracing::info!("AudioOutput: cpal stream.play() succeeded");

    let device_name = device
        .name()
        .unwrap_or_else(|_| "Unknown Device".to_string());

    Ok((
        ResolvedOutputConfig {
            device_name,
            sample_rate: device_sample_rate,
            channels: device_channels,
            buffer_size: device_buffer_size,
        },
        stream,
    ))
}
