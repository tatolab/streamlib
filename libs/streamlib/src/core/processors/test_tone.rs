//! Test tone generator processor
//!
//! Generates sine wave test tones for audio testing and validation.
//! Useful for testing audio output without requiring microphone input.

use crate::core::{AudioFrame, Result, StreamProcessor, StreamOutput};
use std::f64::consts::PI;

/// Output ports for TestToneGenerator
pub struct TestToneGeneratorOutputPorts {
    /// Audio output port (sends AudioFrame)
    pub audio: StreamOutput<AudioFrame>,
}

/// Test tone generator processor
///
/// Generates a continuous sine wave at a specified frequency.
/// Useful for testing audio output processors and validating the audio pipeline.
///
/// Wakes up periodically via TimerRequirements to generate audio buffers at the optimal rate.
///
/// # Example
///
/// ```ignore
/// use streamlib::TestToneGenerator;
///
/// // Generate 440Hz (A4) tone at 48kHz stereo with 50% volume
/// let mut tone_gen = TestToneGenerator::new(440.0, 48000, 0.5);
///
/// // Connect to output
/// runtime.connect(&mut tone_gen.output_ports().audio, &mut speaker.input_ports().audio)?;
/// ```
pub struct TestToneGenerator {
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
    const BUFFER_SIZE: usize = 2048;

    /// Create new test tone generator
    ///
    /// Generates audio buffers at the optimal rate for the given sample rate.
    /// Uses TimerRequirements to wake up periodically.
    ///
    /// # Arguments
    ///
    /// * `frequency` - Frequency in Hz (e.g., 440.0 for A4 note)
    /// * `sample_rate` - Sample rate in Hz (e.g., 48000)
    /// * `amplitude` - Volume (0.0 to 1.0, where 0.5 is 50% volume)
    ///
    /// # Example
    ///
    /// ```
    /// use streamlib::TestToneGenerator;
    ///
    /// // 440Hz tone at 48kHz, 50% volume
    /// let gen = TestToneGenerator::new(440.0, 48000, 0.5);
    /// ```
    pub fn new(frequency: f64, sample_rate: u32, amplitude: f64) -> Self {
        Self {
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
    /// Generates buffer_size samples at the configured frequency and amplitude.
    pub fn generate_frame(&mut self, timestamp_ns: i64) -> AudioFrame {
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

impl StreamProcessor for TestToneGenerator {
    type Config = crate::core::config::TestToneConfig;

    fn from_config(config: Self::Config) -> crate::core::Result<Self> {
        let mut gen = Self::new(
            config.frequency,
            config.sample_rate,
            config.amplitude,
        );
        gen.timer_group_id = config.timer_group_id;
        Ok(gen)
    }

    fn descriptor() -> Option<crate::core::schema::ProcessorDescriptor> {
        use crate::core::schema::{ProcessorDescriptor, AudioRequirements};

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
            .with_tags(vec!["audio", "generator", "test", "real-time"])
        )
    }

    fn descriptor_instance(&self) -> Option<crate::core::schema::ProcessorDescriptor> {
        use crate::core::schema::TimerRequirements;

        // Get base descriptor and add instance-specific TimerRequirements
        Self::descriptor().map(|desc| {
            desc.with_timer_requirements(TimerRequirements {
                rate_hz: self.timer_rate_hz(),
                group_id: self.timer_group_id.clone(),
                description: Some(format!(
                    "Generate audio buffers at {} Hz ({} samples at {} Hz sample rate)",
                    self.timer_rate_hz(),
                    self.buffer_size,
                    self.sample_rate
                )),
            })
        })
    }

    fn process(&mut self) -> Result<()> {
        // Generate audio buffer on every timer tick
        // Timer rate is set via TimerRequirements to match optimal audio generation rate
        tracing::debug!("TestToneGenerator: process() called, generating frame {}", self.frame_number);

        let timestamp_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;

        let frame = self.generate_frame(timestamp_ns);
        self.output_ports.audio.write(frame);

        Ok(())
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn take_output_consumer(&mut self, port_name: &str) -> Option<crate::core::stream_processor::PortConsumer> {
        // TestToneGenerator only has audio output
        match port_name {
            "audio" => {
                self.output_ports.audio.consumer_holder().lock().take()
                    .map(crate::core::stream_processor::PortConsumer::Audio)
            }
            _ => None,
        }
    }

    fn connect_input_consumer(&mut self, _port_name: &str, _consumer: crate::core::stream_processor::PortConsumer) -> bool {
        // TestToneGenerator has no inputs - it's a source processor
        false
    }

    fn set_output_wakeup(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>) {
        match port_name {
            "audio" => self.output_ports.audio.set_downstream_wakeup(wakeup_tx),
            _ => {}
        }
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
}
