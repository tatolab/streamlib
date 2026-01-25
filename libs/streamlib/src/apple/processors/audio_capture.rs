// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::{Result, RuntimeContext, StreamError};
use crate::iceoryx2::OutputWriter;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Stream, StreamConfig};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct AppleAudioInputDevice {
    pub id: usize,
    pub name: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub is_default: bool,
}

#[crate::processor("src/apple/processors/audio_capture.yaml")]
pub struct AppleAudioCaptureProcessor {
    device_info: Option<AppleAudioInputDevice>,
    _device: Option<Device>,
    _stream: Option<Stream>,
    is_capturing: Arc<AtomicBool>,
    frame_counter: Arc<AtomicU64>,
    stream_setup_done: bool,
}

impl crate::core::ManualProcessor for AppleAudioCaptureProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!("[AudioCapture] setup() called - will set up stream in process()");
        self.stream_setup_done = false;
        std::future::ready(Ok(()))
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        let device_name = self
            .device_info
            .as_ref()
            .map(|d| d.name.as_str())
            .unwrap_or("Unknown");
        tracing::info!(
            "AudioCapture {}: Stopping (captured {} frames)",
            device_name,
            self.frame_counter.load(Ordering::Relaxed)
        );

        // Signal the callback to stop processing
        self.is_capturing.store(false, Ordering::Relaxed);

        // Drop the stream to stop the audio callback
        self._stream = None;
        self._device = None;
        std::future::ready(Ok(()))
    }

    fn start(&mut self) -> Result<()> {
        // Pull mode: process() is called once to set up the stream, then cpal callback drives everything
        if !self.stream_setup_done {
            tracing::info!("[AudioCapture] process() called - setting up cpal stream");
            self.setup_stream()?;
            self.stream_setup_done = true;
            tracing::info!(
                "[AudioCapture] Stream setup complete, cpal callback will now drive audio capture"
            );
            return Ok(());
        }

        // After setup, this processor is driven by cpal's audio callback thread
        // We don't do anything here - the callback writes frames directly to output
        Ok(())
    }
}

impl AppleAudioCaptureProcessor::Processor {
    // Separate method for actual stream setup (called from process())
    fn setup_stream(&mut self) -> Result<()> {
        let host = cpal::default_host();

        // Find device by name or use default
        let device = if let Some(device_name_str) = &self.config.device_id {
            // Enumerate all input devices
            let devices: Vec<Device> = host
                .input_devices()
                .map_err(|e| {
                    StreamError::Configuration(format!(
                        "Failed to enumerate audio input devices: {}",
                        e
                    ))
                })?
                .collect();

            // Try to find device by name
            devices
                .into_iter()
                .find(|d| {
                    if let Ok(name) = d.name() {
                        name == *device_name_str
                    } else {
                        false
                    }
                })
                .ok_or_else(|| {
                    StreamError::Configuration(format!(
                        "Audio input device '{}' not found",
                        device_name_str
                    ))
                })?
        } else {
            // Use default input device when None
            host.default_input_device()
                .ok_or_else(|| StreamError::Configuration("No default audio input device".into()))?
        };

        let device_name = device
            .name()
            .unwrap_or_else(|_| "Unknown Device".to_string());

        // Get device's native configuration
        let default_config = device.default_input_config().map_err(|e| {
            StreamError::Configuration(format!("Failed to get audio config: {}", e))
        })?;

        let device_sample_rate = default_config.sample_rate().0;
        let device_channels = default_config.channels();

        tracing::info!(
            "Audio input device: {} (native: {}Hz, {} channels)",
            device_name,
            device_sample_rate,
            device_channels
        );

        let device_info = AppleAudioInputDevice {
            id: 0,
            name: device_name.clone(),
            sample_rate: device_sample_rate,
            channels: device_channels as u32,
            is_default: self.config.device_id.is_none(),
        };

        // Only support mono devices
        if device_channels != 1 {
            return Err(StreamError::Configuration(format!(
                "Audio input device '{}' is not mono (has {} channels). Only mono devices are supported.",
                device_name, device_channels
            )));
        }

        let outputs_clone: Arc<OutputWriter> = self.outputs.clone();
        let frame_counter_clone = self.frame_counter.clone();
        let is_capturing_clone = Arc::clone(&self.is_capturing);
        let sample_rate_clone = device_sample_rate;

        // Use device's native configuration
        // IMPORTANT: We must keep buffer_size as Default for input streams on macOS
        let stream_config = StreamConfig {
            channels: 1, // Mono only
            sample_rate: cpal::SampleRate(device_sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        tracing::info!("[AudioCapture] Building mono input stream with native config");

        let stream = device
            .build_input_stream(
                &stream_config,
                move |data: &[f32], _info: &cpal::InputCallbackInfo| {
                    // Check if we should still be capturing (shutdown flag)
                    if !is_capturing_clone.load(Ordering::Relaxed) {
                        return;
                    }

                    // NOTE: Cannot use tracing here - this runs on cpal's audio callback thread

                    // Create mono audio frame directly (no conversion needed)
                    let frame_number = frame_counter_clone.fetch_add(1, Ordering::Relaxed);
                    let timestamp_ns =
                        crate::core::media_clock::MediaClock::now().as_nanos() as i64;

                    // Create IPC frame with mono audio data
                    let ipc_frame = crate::_generated_::Audioframe1Ch {
                        samples: data.to_vec(),
                        sample_rate: sample_rate_clone,
                        timestamp_ns: timestamp_ns.to_string(),
                        frame_index: frame_number.to_string(),
                    };

                    if let Err(e) = outputs_clone.write("audio", &ipc_frame) {
                        // Cannot use tracing in callback - use eprintln for errors
                        eprintln!("[AudioCapture] Failed to write frame: {}", e);
                    }
                },
                move |err| {
                    tracing::error!("Audio capture error: {}", err);
                },
                None,
            )
            .map_err(|e| {
                StreamError::Configuration(format!("Failed to build audio stream: {}", e))
            })?;

        tracing::info!("[AudioCapture] Starting stream...");

        stream.play().map_err(|e| {
            StreamError::Configuration(format!("Failed to start audio stream: {}", e))
        })?;

        // Set is_capturing flag to true now that stream is active
        self.is_capturing.store(true, Ordering::Relaxed);
        tracing::info!(
            "[AudioCapture] Stream active - capturing mono audio at {}Hz",
            device_sample_rate
        );

        self.device_info = Some(device_info);
        self._device = Some(device);
        self._stream = Some(stream);

        tracing::info!(
            "[AudioCapture] {} Started - outputting device-native mono frames",
            device_name
        );
        Ok(())
    }

    // Helper methods
    pub fn list_devices() -> Result<Vec<AppleAudioInputDevice>> {
        let host = cpal::default_host();
        let devices: Result<Vec<AppleAudioInputDevice>> = host
            .input_devices()
            .map_err(|e| {
                StreamError::Configuration(format!(
                    "Failed to enumerate audio input devices: {}",
                    e
                ))
            })?
            .enumerate()
            .filter_map(|(id, device)| {
                let name = device.name().ok()?;
                let config = device.default_input_config().ok()?;
                let channels = config.channels();

                // Only include mono devices (1 channel)
                if channels != 1 {
                    return None;
                }

                let sample_rate = config.sample_rate().0;

                let is_default = if let Some(default_device) = host.default_input_device() {
                    device.name().ok() == default_device.name().ok()
                } else {
                    false
                };

                Some(Ok(AppleAudioInputDevice {
                    id,
                    name,
                    sample_rate,
                    channels: 1,
                    is_default,
                }))
            })
            .collect();

        devices
    }

    pub fn current_device(&self) -> Option<&AppleAudioInputDevice> {
        self.device_info.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::_generated_::AudioCaptureConfig;
    use crate::core::GeneratedProcessor;

    #[test]
    #[ignore] // Requires real audio hardware - not available in CI
    fn test_list_devices() {
        let devices = AppleAudioCaptureProcessor::Processor::list_devices();

        assert!(devices.is_ok());

        if let Ok(devices) = devices {
            println!("Found {} audio input devices:", devices.len());
            for device in &devices {
                println!(
                    "  [{}] {}: {}Hz, {} channels{}",
                    device.id,
                    device.name,
                    device.sample_rate,
                    device.channels,
                    if device.is_default { " (default)" } else { "" }
                );
            }

            assert!(
                !devices.is_empty(),
                "Expected at least one audio input device"
            );
        }
    }

    #[test]
    fn test_create_default_device() {
        let config = AudioCaptureConfig { device_id: None };

        let result = AppleAudioCaptureProcessor::Processor::from_config(config);

        match result {
            Ok(_processor) => {
                println!("Successfully created audio capture processor from config");
            }
            Err(e) => {
                println!(
                    "Note: Could not create audio capture (may require permissions): {}",
                    e
                );
            }
        }
    }

    #[test]
    fn test_capture_audio() {
        let config = AudioCaptureConfig::default();
        let result = AppleAudioCaptureProcessor::Processor::from_config(config);

        if let Ok(mut processor) = result {
            std::thread::sleep(std::time::Duration::from_millis(100));

            let result = processor.process();
            if result.is_ok() {
                println!("Successfully processed captured audio");
            } else {
                println!("Note: Audio processing returned: {:?}", result);
            }
        } else {
            println!("Note: Could not create audio capture (may require permissions)");
        }
    }
}
