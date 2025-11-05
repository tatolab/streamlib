use crate::core::{
    Result, StreamError, StreamInput, StreamOutput,
    ProcessorDescriptor, PortDescriptor, AudioRequirements,
};
use crate::core::frames::AudioFrame;
use crate::core::bus::PortMessage;
use crate::core::traits::{StreamElement, StreamProcessor, ElementType};
use serde::{Serialize, Deserialize};
use dasp::Signal;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioMixerConfig {
    pub strategy: MixingStrategy,
    /// Timestamp alignment tolerance in milliseconds
    /// If Some(ms), mixer will wait until all input timestamps are within ±ms
    /// If None, no timestamp checking (legacy behavior)
    pub timestamp_tolerance_ms: Option<u32>,
}

impl Default for AudioMixerConfig {
    fn default() -> Self {
        Self {
            strategy: MixingStrategy::SumNormalized,
            timestamp_tolerance_ms: Some(10), // Default: 10ms tolerance
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MixingStrategy {
    Sum,
    SumNormalized,
    SumClipped,
}

impl Default for MixingStrategy {
    fn default() -> Self {
        MixingStrategy::SumNormalized
    }
}

pub struct AudioMixerProcessor<const N: usize> {
    strategy: MixingStrategy,
    timestamp_tolerance_ms: Option<u32>,
    sample_rate: u32,
    buffer_size: usize,
    frame_counter: u64,

    pub input_ports: [StreamInput<AudioFrame<1>>; N],

    pub output_ports: AudioMixerOutputPorts,
}

pub struct AudioMixerOutputPorts {
    pub audio: StreamOutput<AudioFrame<2>>,
}

impl<const N: usize> AudioMixerProcessor<N> {
    pub fn new(strategy: MixingStrategy) -> Result<Self> {
        Self::new_with_tolerance(strategy, Some(10))
    }

    pub fn new_with_tolerance(strategy: MixingStrategy, timestamp_tolerance_ms: Option<u32>) -> Result<Self> {
        if N == 0 {
            return Err(StreamError::Configuration(
                "AudioMixerProcessor requires at least 1 input".into()
            ));
        }

        let input_ports: [StreamInput<AudioFrame<1>>; N] = std::array::from_fn(|i| {
            StreamInput::new(format!("input_{}", i))
        });

        Ok(Self {
            strategy,
            timestamp_tolerance_ms,
            sample_rate: 48000,
            buffer_size: 128,
            frame_counter: 0,
            input_ports,
            output_ports: AudioMixerOutputPorts {
                audio: StreamOutput::new("audio"),
            },
        })
    }
}

impl<const N: usize> StreamElement for AudioMixerProcessor<N> {
    fn name(&self) -> &str {
        "audio_mixer"
    }

    fn element_type(&self) -> ElementType {
        ElementType::Transform
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <Self as StreamProcessor>::descriptor()
    }

    fn input_ports(&self) -> Vec<PortDescriptor> {
        (0..N)
            .map(|i| PortDescriptor {
                name: format!("input_{}", i),
                schema: AudioFrame::<1>::schema(),
                required: true,
                description: format!("Mono audio input {} for mixing", i),
            })
            .collect()
    }

    fn output_ports(&self) -> Vec<PortDescriptor> {
        vec![PortDescriptor {
            name: "audio".to_string(),
            schema: AudioFrame::<2>::schema(),
            required: true,
            description: "Mixed stereo audio output".to_string(),
        }]
    }

    fn start(&mut self, ctx: &crate::core::RuntimeContext) -> Result<()> {
        self.sample_rate = ctx.audio.sample_rate;
        self.buffer_size = ctx.audio.buffer_size;
        self.frame_counter = 0;

        tracing::info!(
            "AudioMixer<{}>: Starting ({} Hz, {} samples buffer, strategy: {:?})",
            N,
            self.sample_rate,
            self.buffer_size,
            self.strategy
        );
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        tracing::info!("AudioMixer<{}>: Stopped", N);
        Ok(())
    }
}

impl<const N: usize> StreamProcessor for AudioMixerProcessor<N> {
    type Config = AudioMixerConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        Self::new_with_tolerance(config.strategy, config.timestamp_tolerance_ms)
    }

    fn descriptor() -> Option<ProcessorDescriptor> {
        Some(
            ProcessorDescriptor::new(
                &format!("AudioMixerProcessor<{}>", N),
                &format!("Mixes {} mono signals into a stereo signal using dasp", N)
            )
            .with_usage_context(
                "Use when you need to combine multiple mono audio sources into a stereo stream. \
                 All mixing is performed using lazy dasp signal combinators - zero-copy until samples are consumed. \
                 Input channels are compile-time constant for type safety."
            )
            .with_audio_requirements(AudioRequirements {
                preferred_buffer_size: Some(2048),
                required_buffer_size: None,
                supported_sample_rates: vec![44100, 48000, 96000],
                required_channels: Some(2),
            })
            .with_tags(vec!["audio", "mixer", "transform", "multi-input", "dasp"])
        )
    }

    fn process(&mut self) -> Result<()> {
        tracing::info!("[AudioMixer<{}>] process() called", N);

        // Check if timestamp alignment is enabled
        if let Some(tolerance_ms) = self.timestamp_tolerance_ms {
            // Timestamp-based synchronization
            let tolerance_ns = tolerance_ms as i64 * 1_000_000;

            // Peek at all timestamps WITHOUT consuming
            let peeked_frames: [Option<AudioFrame<1>>; N] = std::array::from_fn(|i| {
                self.input_ports[i].peek()
            });

            // Check if all inputs have data
            if peeked_frames.iter().any(|f| f.is_none()) {
                tracing::debug!("[AudioMixer<{}>] Waiting for all inputs to have data", N);
                return Ok(());
            }

            // Extract timestamps
            let timestamps: [i64; N] = std::array::from_fn(|i| {
                peeked_frames[i].as_ref().unwrap().timestamp_ns
            });

            // Check timestamp alignment
            let min_ts = *timestamps.iter().min().unwrap();
            let max_ts = *timestamps.iter().max().unwrap();
            let spread_ns = max_ts - min_ts;

            if spread_ns > tolerance_ns {
                tracing::debug!(
                    "[AudioMixer<{}>] Timestamps not aligned: spread={}ms (tolerance={}ms), min={}, max={}",
                    N,
                    spread_ns / 1_000_000,
                    tolerance_ms,
                    min_ts,
                    max_ts
                );

                // If spread is excessive (>100ms), drop old frames to catch up
                let max_drift_ns = 100_000_000; // 100ms maximum drift before dropping
                if spread_ns > max_drift_ns {
                    tracing::warn!(
                        "[AudioMixer<{}>] Excessive timestamp drift ({}ms), dropping old frames",
                        N,
                        spread_ns / 1_000_000
                    );

                    // Drop frames from inputs that are behind
                    for (i, &ts) in timestamps.iter().enumerate() {
                        if ts < max_ts - tolerance_ns {
                            // This input is lagging - drop its frame
                            let dropped = self.input_ports[i].read_latest();
                            tracing::debug!(
                                "[AudioMixer<{}>] Dropped frame from input {}: ts={} ({}ms behind)",
                                N,
                                i,
                                ts,
                                (max_ts - ts) / 1_000_000
                            );
                            // Verify we actually dropped something
                            if dropped.is_none() {
                                tracing::error!(
                                    "[AudioMixer<{}>] Failed to drop frame from input {} (peek showed data but read returned None)",
                                    N,
                                    i
                                );
                            }
                        }
                    }
                }

                return Ok(());
            }

            // All aligned - consume frames
            let input_frames: [AudioFrame<1>; N] = std::array::from_fn(|i| {
                self.input_ports[i].read_latest().expect("checked via peek")
            });

            tracing::debug!(
                "[AudioMixer<{}>] Timestamps aligned (spread={}ms), mixing",
                N,
                spread_ns / 1_000_000
            );

            // Mix and output
            self.mix_frames(&input_frames)
        } else {
            // Legacy mode: no timestamp checking
            let all_ready = self.input_ports.iter().all(|input| input.has_data());
            if !all_ready {
                tracing::debug!("[AudioMixer<{}>] Not all inputs have data yet, skipping", N);
                return Ok(());
            }

            let input_frames: [AudioFrame<1>; N] = std::array::from_fn(|i| {
                self.input_ports[i].read_latest().expect("checked has_data")
            });

            self.mix_frames(&input_frames)
        }
    }

    fn set_output_wakeup(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>) {
        if port_name == "audio" {
            self.output_ports.audio.set_downstream_wakeup(wakeup_tx);
        }
    }

    fn get_output_port_type(&self, port_name: &str) -> Option<crate::core::bus::PortType> {
        match port_name {
            "audio" => Some(crate::core::bus::PortType::Audio2),
            _ => None,
        }
    }

    fn get_input_port_type(&self, port_name: &str) -> Option<crate::core::bus::PortType> {
        if let Some(index_str) = port_name.strip_prefix("input_") {
            if let Ok(index) = index_str.parse::<usize>() {
                if index < N {
                    return Some(crate::core::bus::PortType::Audio1);
                }
            }
        }
        None
    }

    fn wire_output_connection(&mut self, port_name: &str, connection: std::sync::Arc<dyn std::any::Any + Send + Sync>) -> bool {
        use crate::core::bus::ProcessorConnection;
        use crate::core::AudioFrame;

        if let Ok(typed_conn) = connection.downcast::<std::sync::Arc<ProcessorConnection<AudioFrame<2>>>>() {
            if port_name == "audio" {
                self.output_ports.audio.add_connection(std::sync::Arc::clone(&typed_conn));
                return true;
            }
        }
        false
    }

    fn wire_input_connection(&mut self, port_name: &str, connection: std::sync::Arc<dyn std::any::Any + Send + Sync>) -> bool {
        use crate::core::bus::ProcessorConnection;
        use crate::core::AudioFrame;

        if let Ok(typed_conn) = connection.downcast::<std::sync::Arc<ProcessorConnection<AudioFrame<1>>>>() {
            if let Some(index_str) = port_name.strip_prefix("input_") {
                if let Ok(index) = index_str.parse::<usize>() {
                    if index < N {
                        self.input_ports[index].set_connection(std::sync::Arc::clone(&typed_conn));
                        return true;
                    }
                }
            }
        }
        false
    }
}

// Helper methods outside trait impl
impl<const N: usize> AudioMixerProcessor<N> {
    fn mix_frames(&mut self, input_frames: &[AudioFrame<1>; N]) -> Result<()> {
        let timestamp_ns = input_frames[0].timestamp_ns;

        let mut signals: Vec<_> = input_frames.iter()
            .map(|frame| frame.read())
            .collect();

        let mut mixed_samples = Vec::with_capacity(self.buffer_size * 2); // *2 for stereo

        for _ in 0..self.buffer_size {
            let mut mixed_mono = 0.0f32;
            for signal in &mut signals {
                mixed_mono += signal.next()[0];
            }

            let final_sample = match self.strategy {
                MixingStrategy::Sum => mixed_mono,
                MixingStrategy::SumNormalized => mixed_mono / N as f32,
                MixingStrategy::SumClipped => mixed_mono.clamp(-1.0, 1.0),
            };

            mixed_samples.push(final_sample);  // Left
            mixed_samples.push(final_sample);  // Right
        }

        let output_frame = AudioFrame::<2>::new(mixed_samples, timestamp_ns, self.frame_counter);

        self.output_ports.audio.write(output_frame);
        tracing::debug!("[AudioMixer<{}>] Wrote mixed stereo frame", N);

        self.frame_counter += 1;

        Ok(())
    }
}
