// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::{Arc, Mutex};

use crate::_generated_::Audioframe;
use crate::core::utils::audio_resample::{AudioResampler, ResamplingQuality};
use crate::core::utils::audio_utils::{convert_channels, AudioRechunker};
use crate::core::Result;

/// Target audio format for conversion.
pub struct ProcessorAudioConverterTargetFormat {
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
    pub buffer_size: Option<usize>,
}

/// Observable status of audio conversion operations.
pub struct ProcessorAudioConverterStatus {
    pub source_sample_rate: Option<u32>,
    pub source_channels: Option<u8>,
    pub target_sample_rate: Option<u32>,
    pub target_channels: Option<u8>,
    pub target_buffer_size: Option<usize>,
    pub frames_converted: u64,
    pub is_resampling: bool,
    pub is_converting_channels: bool,
    pub is_rechunking: bool,
}

impl Default for ProcessorAudioConverterStatus {
    fn default() -> Self {
        Self {
            source_sample_rate: None,
            source_channels: None,
            target_sample_rate: None,
            target_channels: None,
            target_buffer_size: None,
            frames_converted: 0,
            is_resampling: false,
            is_converting_channels: false,
            is_rechunking: false,
        }
    }
}

/// Per-processor audio format converter with lazy-initialized resampler and rechunker.
pub struct ProcessorAudioConverter {
    resampler: Option<AudioResampler>,
    resampler_chunk_size: usize,
    pre_resample_buffer: Vec<f32>,
    rechunker: Option<AudioRechunker>,
    last_source_sample_rate: Option<u32>,
    last_source_channels: Option<u8>,
    last_target: Option<StoredTargetFormat>,
    status: Arc<Mutex<ProcessorAudioConverterStatus>>,
}

/// Stored copy of target format for detecting changes.
#[allow(dead_code)]
struct StoredTargetFormat {
    sample_rate: Option<u32>,
    channels: Option<u8>,
    buffer_size: Option<usize>,
}

impl ProcessorAudioConverter {
    /// Create a new converter. Near-zero cost — no resampler or rechunker allocated.
    pub fn new() -> Self {
        Self {
            resampler: None,
            resampler_chunk_size: 0,
            pre_resample_buffer: Vec::new(),
            rechunker: None,
            last_source_sample_rate: None,
            last_source_channels: None,
            last_target: None,
            status: Arc::new(Mutex::new(ProcessorAudioConverterStatus::default())),
        }
    }

    /// Returns the shared status Arc for the ECS component.
    pub fn status_arc(&self) -> Arc<Mutex<ProcessorAudioConverterStatus>> {
        Arc::clone(&self.status)
    }

    /// Convert an audio frame to the target format.
    ///
    /// Returns 0 or more output frames (0 when accumulating for resampler/rechunker).
    pub fn convert(
        &mut self,
        frame: &Audioframe,
        target: &ProcessorAudioConverterTargetFormat,
    ) -> Result<Vec<Audioframe>> {
        let source_sample_rate = frame.sample_rate;
        let source_channels = frame.channels;

        // Detect source format change — re-initialize everything
        let source_changed = self.last_source_sample_rate != Some(source_sample_rate)
            || self.last_source_channels != Some(source_channels);

        if source_changed {
            self.resampler = None;
            self.resampler_chunk_size = 0;
            self.pre_resample_buffer.clear();
            self.rechunker = None;
            self.last_source_sample_rate = Some(source_sample_rate);
            self.last_source_channels = Some(source_channels);
        }

        // Step 1: Channel conversion
        let needs_channel_conversion = target
            .channels
            .map(|tc| tc != source_channels)
            .unwrap_or(false);

        let after_channels = if needs_channel_conversion {
            convert_channels(frame, target.channels.unwrap())
        } else {
            frame.clone()
        };

        let channels_after_conversion = after_channels.channels;

        // Step 2: Resampling (with pre-resampler accumulation buffer)
        let needs_resampling = target
            .sample_rate
            .map(|ts| ts != after_channels.sample_rate)
            .unwrap_or(false);

        let resampled_frames = if needs_resampling {
            let target_rate = target.sample_rate.unwrap();
            let ch = channels_after_conversion as usize;

            // Lazy-init resampler using target buffer_size to derive chunk_size.
            // The resampler input chunk_size is chosen so that after resampling,
            // output is approximately target buffer_size samples per channel.
            if self.resampler.is_none() {
                let ratio = target_rate as f64 / after_channels.sample_rate as f64;
                let chunk_size = if let Some(buf_size) = target.buffer_size {
                    // Derive input chunk from desired output size and resample ratio
                    ((buf_size as f64) / ratio).ceil() as usize
                } else {
                    // No buffer_size target — use the incoming frame's size
                    after_channels.samples.len() / ch
                };

                tracing::info!(
                    "[ProcessorAudioConverter] Initializing resampler: {}Hz -> {}Hz ({} channels, chunk_size={})",
                    after_channels.sample_rate,
                    target_rate,
                    channels_after_conversion,
                    chunk_size
                );
                let resampler = AudioResampler::new(
                    after_channels.sample_rate,
                    target_rate,
                    channels_after_conversion,
                    chunk_size,
                    ResamplingQuality::Medium,
                )?;
                self.resampler = Some(resampler);
                self.resampler_chunk_size = chunk_size;
            }

            let chunk_size = self.resampler_chunk_size;
            let interleaved_chunk_size = chunk_size * ch;

            // Accumulate samples in the pre-resampler buffer
            self.pre_resample_buffer
                .extend_from_slice(&after_channels.samples);

            // Feed the resampler in exact chunk_size portions
            let mut resampled_output: Vec<Audioframe> = Vec::new();

            while self.pre_resample_buffer.len() >= interleaved_chunk_size {
                let chunk: Vec<f32> = self
                    .pre_resample_buffer
                    .drain(..interleaved_chunk_size)
                    .collect();

                let resampled_samples = self.resampler.as_mut().unwrap().resample(&chunk)?;

                resampled_output.push(Audioframe {
                    samples: resampled_samples,
                    channels: channels_after_conversion,
                    sample_rate: target_rate,
                    timestamp_ns: after_channels.timestamp_ns.clone(),
                    frame_index: after_channels.frame_index.clone(),
                });
            }

            resampled_output
        } else {
            vec![after_channels]
        };

        // Step 3: Rechunking
        let needs_rechunking = target.buffer_size.is_some();

        let output_frames = if needs_rechunking {
            let buffer_size = target.buffer_size.unwrap();

            // Lazy-init rechunker
            if self.rechunker.is_none() {
                let out_channels = if !resampled_frames.is_empty() {
                    resampled_frames[0].channels
                } else {
                    target.channels.unwrap_or(source_channels)
                };
                tracing::info!(
                    "[ProcessorAudioConverter] Initializing rechunker: {} samples/channel, {} channels",
                    buffer_size,
                    out_channels
                );
                self.rechunker = Some(AudioRechunker::new(out_channels, buffer_size));
            }

            let rechunker = self.rechunker.as_mut().unwrap();
            let mut frames = Vec::new();

            for resampled in &resampled_frames {
                // Feed frame and drain all complete chunks
                if let Some(f) = rechunker.process(resampled) {
                    frames.push(f);
                    // Pump remaining buffered data
                    let empty = Audioframe {
                        samples: vec![],
                        channels: resampled.channels,
                        sample_rate: resampled.sample_rate,
                        timestamp_ns: resampled.timestamp_ns.clone(),
                        frame_index: resampled.frame_index.clone(),
                    };
                    loop {
                        match rechunker.process(&empty) {
                            Some(f) => frames.push(f),
                            None => break,
                        }
                    }
                }
            }

            frames
        } else {
            resampled_frames
        };

        // Update status
        {
            let mut status = self.status.lock().unwrap();
            status.source_sample_rate = Some(source_sample_rate);
            status.source_channels = Some(source_channels);
            status.target_sample_rate = target.sample_rate;
            status.target_channels = target.channels;
            status.target_buffer_size = target.buffer_size;
            status.frames_converted += output_frames.len() as u64;
            status.is_resampling = needs_resampling;
            status.is_converting_channels = needs_channel_conversion;
            status.is_rechunking = needs_rechunking;
        }

        self.last_target = Some(StoredTargetFormat {
            sample_rate: target.sample_rate,
            channels: target.channels,
            buffer_size: target.buffer_size,
        });

        Ok(output_frames)
    }
}

impl Default for ProcessorAudioConverter {
    fn default() -> Self {
        Self::new()
    }
}
