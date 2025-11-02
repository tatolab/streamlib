//! Test tone generator source processor
//!
//! Generates sine wave test tones for audio testing and validation.
//! Useful for testing audio output without requiring microphone input.
//!
//! This is a **source processor** - it generates data without consuming inputs.

use crate::core::traits::{StreamElement, StreamSource, ElementType, SchedulingConfig, SchedulingMode, ClockSource};
use crate::core::{AudioFrame, Result, StreamOutput, StreamProcessor, GpuContext};
use crate::core::schema::{ProcessorDescriptor, PortDescriptor, AudioRequirements, TimerRequirements, SCHEMA_AUDIO_FRAME};
use crate::core::config::TestToneConfig;
use std::f64::consts::PI;

/// Output ports for TestToneGenerator
pub struct TestToneGeneratorOutputPorts {
    /// Audio output port (sends AudioFrame)
    pub audio: StreamOutput<AudioFrame>,
}

/// Test tone generator source processor
///
/// Generates a continuous sine wave at a specified frequency.
/// Useful for testing audio output processors and validating the audio pipeline.
///
/// Implements the **StreamSource** trait - runs in a loop generating audio buffers.
///
/// # Example
///
/// ```ignore
/// use streamlib::{TestToneGenerator, TestToneConfig, StreamRuntime};
///
/// let mut runtime = StreamRuntime::new();
///
/// let tone = runtime.add_processor_with_config::<TestToneGenerator>(
///     TestToneConfig {
///         frequency: 440.0,
///         amplitude: 0.5,
///         sample_rate: 48000,
///         timer_group_id: None,
///     }
/// )?;
///
/// runtime.start().await?;
/// ```
pub struct TestToneGenerator {
    /// Processor name
    name: String,

    /// Frequency in Hz (e.g., 440.0 for A4)
    frequency: f64,

    /// Sample rate in Hz (e.g., 48000)
    sample_rate: u32,

    /// Number of channels (always stereo for compatibility)
    channels: u32,

    /// Current phase in the sine wave (0.0 to 2π)
    phase: f64,

    /// Amplitude (0.0 to 1.0)
    amplitude: f64,

    /// Frame counter
    frame_number: u64,

    /// Samples per buffer (fixed at 2048 for optimal audio processing)
    buffer_size: usize,

    /// Optional timer group ID for synchronized wakeups
    timer_group_id: Option<String>,

    /// Output ports
    output_ports: TestToneGeneratorOutputPorts,
}

impl TestToneGenerator {
    /// Fixed buffer size for audio generation (2048 samples is standard)
    pub const BUFFER_SIZE: usize = 2048;

    /// Create new test tone generator
    ///
    /// # Arguments
    ///
    /// * `frequency` - Frequency in Hz (e.g., 440.0 for A4 note)
    /// * `sample_rate` - Sample rate in Hz (e.g., 48000)
    /// * `amplitude` - Volume (0.0 to 1.0, where 0.5 is 50% volume)
    pub fn new(frequency: f64, sample_rate: u32, amplitude: f64) -> Self {
        Self {
            name: "test_tone".to_string(),
            frequency,
            sample_rate,
            channels: 2, // Always stereo for compatibility
            phase: 0.0,
            amplitude: amplitude.clamp(0.0, 1.0),
            frame_number: 0,
            buffer_size: Self::BUFFER_SIZE,
            timer_group_id: None,
            output_ports: TestToneGeneratorOutputPorts {
                audio: StreamOutput::new("audio"),
            },
        }
    }

    /// Calculate optimal timer rate for this generator
    ///
    /// Returns the rate in Hz at which this processor should wake up.
    /// For 48kHz with 2048 sample buffers: 48000 / 2048 ≈ 23.44 Hz
    fn timer_rate_hz(&self) -> f64 {
        self.sample_rate as f64 / self.buffer_size as f64
    }

    /// Get mutable access to output ports
    ///
    /// Required for type-safe connections between processors.
    pub fn output_ports(&mut self) -> &mut TestToneGeneratorOutputPorts {
        &mut self.output_ports
    }

    /// Set amplitude (0.0 to 1.0)
    pub fn set_amplitude(&mut self, amplitude: f64) {
        self.amplitude = amplitude.clamp(0.0, 1.0);
    }

    /// Generate next audio buffer
    ///
    /// Called by the runtime's source loop.
    /// Generates buffer_size samples at the configured frequency and amplitude.
    fn generate_frame(&mut self, timestamp_ns: i64) -> AudioFrame {
        let mut samples = Vec::with_capacity(self.buffer_size * self.channels as usize);

        // Phase increment per sample
        let phase_increment = 2.0 * PI * self.frequency / self.sample_rate as f64;

        // Generate samples
        for _ in 0..self.buffer_size {
            // Calculate sine wave sample
            let sample = (self.phase.sin() * self.amplitude) as f32;

            // Add sample for each channel
            for _ in 0..self.channels {
                samples.push(sample);
            }

            // Increment phase
            self.phase += phase_increment;

            // Wrap phase to prevent floating point drift
            if self.phase >= 2.0 * PI {
                self.phase -= 2.0 * PI;
            }
        }

        let frame = AudioFrame::new(
            samples,
            timestamp_ns,
            self.frame_number,
            self.sample_rate,
            self.channels,
        );

        self.frame_number += 1;

        frame
    }
}

// ============================================================
// StreamElement Implementation (Base Trait)
// ============================================================

impl StreamElement for TestToneGenerator {
    fn name(&self) -> &str {
        &self.name
    }

    fn element_type(&self) -> ElementType {
        ElementType::Source
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <TestToneGenerator as StreamSource>::descriptor()
    }

    fn output_ports(&self) -> Vec<PortDescriptor> {
        vec![PortDescriptor {
            name: "audio".to_string(),
            schema: SCHEMA_AUDIO_FRAME.clone(),
            required: true,
            description: "Generated sine wave audio output".to_string(),
        }]
    }

    fn as_source(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_source_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

// ============================================================
// StreamSource Implementation (Specialized Trait)
// ============================================================

impl StreamSource for TestToneGenerator {
    type Output = AudioFrame;
    type Config = TestToneConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        let mut gen = Self::new(
            config.frequency,
            config.sample_rate,
            config.amplitude,
        );
        gen.timer_group_id = config.timer_group_id;
        Ok(gen)
    }

    fn generate(&mut self) -> Result<Self::Output> {
        tracing::debug!("TestToneGenerator: generate() called, frame {}", self.frame_number);

        let timestamp_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;

        let frame = self.generate_frame(timestamp_ns);

        Ok(frame)
    }

    fn scheduling_config(&self) -> SchedulingConfig {
        SchedulingConfig {
            mode: SchedulingMode::Loop,
            clock: ClockSource::Audio,
            rate_hz: Some(self.timer_rate_hz()),
            provide_clock: false,
        }
    }

    fn descriptor() -> Option<ProcessorDescriptor> {
        Some(
            ProcessorDescriptor::new(
                "TestToneGenerator",
                "Generates sine wave test tones for audio testing and validation"
            )
            .with_usage_context(
                "Use for testing audio output processors without requiring microphone input. \
                 Generates samples synchronized to runtime tick rate for real-time processing. \
                 Can generate tones at any frequency and amplitude."
            )
            .with_audio_requirements(AudioRequirements {
                preferred_buffer_size: None,         // Dynamically calculated from tick rate
                required_buffer_size: None,          // Flexible - adapts to runtime
                supported_sample_rates: vec![],      // Any sample rate supported
                required_channels: None,             // Always outputs stereo
            })
            .with_tags(vec!["audio", "source", "generator", "test", "real-time"])
        )
    }
}

// ============================================================
// StreamProcessor Implementation (Legacy Compatibility)
// ============================================================

impl StreamProcessor for TestToneGenerator {
    type Config = TestToneConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        <Self as StreamSource>::from_config(config)
    }

    fn process(&mut self) -> Result<()> {
        // Phase 6: Delegate to StreamSource::generate() for clean separation
        // generate() produces the frame, process() writes it to the output port
        match <Self as StreamSource>::generate(self) {
            Ok(frame) => {
                self.output_ports.audio.write(frame);
                Ok(())
            }
            Err(e) => {
                tracing::error!("TestToneGenerator: Error generating frame: {:?}", e);
                Err(e)
            }
        }
    }

    fn descriptor() -> Option<ProcessorDescriptor> {
        <Self as StreamSource>::descriptor()
    }

    fn descriptor_instance(&self) -> Option<ProcessorDescriptor> {
        // Get base descriptor and add instance-specific TimerRequirements
        <Self as StreamSource>::descriptor().map(|desc| {
            desc.with_timer_requirements(TimerRequirements {
                rate_hz: self.timer_rate_hz(),
                group_id: self.timer_group_id.clone(),
                description: Some(format!(
                    "Generate audio buffers at {:.2} Hz ({} samples at {} Hz sample rate)",
                    self.timer_rate_hz(),
                    self.buffer_size,
                    self.sample_rate
                )),
            })
        })
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn on_start(&mut self, _gpu_context: &GpuContext) -> Result<()> {
        tracing::info!(
            "[TestToneGenerator] Started: {}Hz tone at {}Hz sample rate",
            self.frequency,
            self.sample_rate
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_tone_generator() {
        let gen = TestToneGenerator::new(440.0, 48000, 0.5);
        assert_eq!(gen.frequency, 440.0);
        assert_eq!(gen.sample_rate, 48000);
        assert_eq!(gen.channels, 2); // Always stereo
        assert_eq!(gen.amplitude, 0.5);
        assert_eq!(gen.phase, 0.0);
        assert_eq!(gen.frame_number, 0);
        assert_eq!(gen.buffer_size, TestToneGenerator::BUFFER_SIZE);
    }

    #[test]
    fn test_from_config() {
        let config = TestToneConfig {
            frequency: 440.0,
            amplitude: 0.5,
            sample_rate: 48000,
            timer_group_id: Some("audio_master".to_string()),
        };
        let gen = <TestToneGenerator as StreamSource>::from_config(config).unwrap();
        assert_eq!(gen.frequency, 440.0);
        assert_eq!(gen.amplitude, 0.5);
        assert_eq!(gen.sample_rate, 48000);
        assert_eq!(gen.timer_group_id, Some("audio_master".to_string()));
    }

    #[test]
    fn test_generate_frame() {
        let mut gen = TestToneGenerator::new(440.0, 48000, 0.5);
        let frame = gen.generate_frame(0);

        // Fixed buffer size: 2048 samples
        assert_eq!(frame.sample_count, 2048);
        assert_eq!(frame.channels, 2); // Always stereo
        assert_eq!(frame.sample_rate, 48000);
        assert_eq!(frame.frame_number, 0);

        // Check samples array size (2048 samples * 2 channels = 4096 total)
        assert_eq!(frame.samples.len(), 4096);

        // Check that samples are non-zero (tone is playing)
        let has_non_zero = frame.samples.iter().any(|&s| s.abs() > 0.0);
        assert!(has_non_zero, "Generated samples should be non-zero");

        // Check that samples are in valid range [-1.0, 1.0]
        for &sample in frame.samples.iter() {
            assert!(
                sample >= -1.0 && sample <= 1.0,
                "Sample {} out of range",
                sample
            );
        }
    }

    #[test]
    fn test_generate() {
        let mut gen = TestToneGenerator::new(440.0, 48000, 0.5);
        let frame = gen.generate().unwrap();
        assert_eq!(frame.sample_count, 2048);
        assert_eq!(frame.samples.len(), 4096);
    }

    #[test]
    fn test_frame_counter_increments() {
        let mut gen = TestToneGenerator::new(440.0, 48000, 0.5);

        let frame1 = gen.generate_frame(0);
        assert_eq!(frame1.frame_number, 0);

        let frame2 = gen.generate_frame(10_000_000); // 10ms later
        assert_eq!(frame2.frame_number, 1);

        let frame3 = gen.generate_frame(20_000_000); // 20ms later
        assert_eq!(frame3.frame_number, 2);
    }

    #[test]
    fn test_amplitude_control() {
        let mut gen = TestToneGenerator::new(440.0, 48000, 1.0);

        // Test at 100% amplitude
        let frame_full = gen.generate_frame(0);
        let max_full = frame_full
            .samples
            .iter()
            .map(|s| s.abs())
            .fold(0.0f32, f32::max);

        // Test at 50% amplitude
        gen.set_amplitude(0.5);
        gen.phase = 0.0; // Reset phase
        gen.frame_number = 0;
        let frame_half = gen.generate_frame(0);
        let max_half = frame_half
            .samples
            .iter()
            .map(|s| s.abs())
            .fold(0.0f32, f32::max);

        // Half amplitude should be roughly half the peak
        assert!(
            max_half < max_full,
            "Half amplitude should be less than full"
        );
        assert!(
            (max_half - max_full * 0.5).abs() < 0.1,
            "Half amplitude should be ~50% of full"
        );
    }

    #[test]
    fn test_stereo_output() {
        let mut gen = TestToneGenerator::new(440.0, 48000, 0.5);
        let frame = gen.generate_frame(0);

        // Fixed buffer: 2048 samples * 2 channels = 4096 total
        assert_eq!(frame.samples.len(), 4096);
        assert_eq!(frame.channels, 2);

        // Stereo should have duplicate samples (L, R pairs)
        for i in (0..frame.samples.len()).step_by(2) {
            assert_eq!(
                frame.samples[i],
                frame.samples[i + 1],
                "Stereo L/R channels should be identical for test tone"
            );
        }
    }

    #[test]
    fn test_phase_continuity() {
        let mut gen = TestToneGenerator::new(440.0, 48000, 0.5);

        // Generate multiple frames
        gen.generate_frame(0);
        gen.generate_frame(10_000_000);
        gen.generate_frame(20_000_000);

        // Phase should have advanced but stayed within [0, 2π)
        assert!(gen.phase >= 0.0);
        assert!(gen.phase < 2.0 * PI);
    }

    #[test]
    fn test_element_type() {
        let gen = TestToneGenerator::new(440.0, 48000, 0.5);
        assert_eq!(gen.element_type(), ElementType::Source);
    }

    #[test]
    fn test_scheduling_config() {
        let gen = TestToneGenerator::new(440.0, 48000, 0.5);
        let sched = gen.scheduling_config();
        assert_eq!(sched.mode, SchedulingMode::Loop);
        assert_eq!(sched.clock, ClockSource::Audio);
        assert_eq!(sched.rate_hz, Some(23.4375)); // 48000 / 2048
        assert!(!sched.provide_clock);
    }

    #[test]
    fn test_output_ports_descriptor() {
        let gen = TestToneGenerator::new(440.0, 48000, 0.5);
        let ports = gen.output_ports();
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].name, "audio");
        assert_eq!(ports[0].schema.name, "AudioFrame");
    }

    #[test]
    fn test_processor_descriptor() {
        let desc = <TestToneGenerator as StreamSource>::descriptor().unwrap();
        assert_eq!(desc.name, "TestToneGenerator");
        assert!(desc.description.contains("sine wave"));
        assert!(desc.tags.contains(&"source".to_string()));
        assert!(desc.tags.contains(&"audio".to_string()));
    }
}
