//! Test tone generator processor
//!
//! Generates sine wave test tones for audio testing and validation.
//! Useful for testing audio output without requiring microphone input.

use crate::core::{AudioFrame, Result, StreamProcessor, TimedTick};
use std::f64::consts::PI;

/// Test tone generator processor
///
/// Generates a continuous sine wave at a specified frequency.
/// Useful for testing audio output processors and validating the audio pipeline.
///
/// # Example
///
/// ```ignore
/// use streamlib::TestToneGenerator;
///
/// // Generate 440Hz (A4) tone at 48kHz stereo
/// let tone_gen = TestToneGenerator::new(440.0, 48000, 2);
///
/// // In process() method, generates AudioFrames
/// let frame = tone_gen.process(tick)?;  // Outputs on "audio" port
/// ```
pub struct TestToneGenerator {
    /// Frequency in Hz (e.g., 440.0 for A4)
    frequency: f64,

    /// Sample rate in Hz (e.g., 48000)
    sample_rate: u32,

    /// Number of channels (1 = mono, 2 = stereo)
    channels: u32,

    /// Current phase in the sine wave (0.0 to 2π)
    phase: f64,

    /// Amplitude (0.0 to 1.0)
    amplitude: f64,

    /// Frame counter
    frame_number: u64,

    /// Samples per frame (based on tick rate)
    samples_per_frame: usize,

    /// Output port name
    #[allow(dead_code)]
    output_port: String,
}

impl TestToneGenerator {
    /// Create new test tone generator
    ///
    /// # Arguments
    ///
    /// * `frequency` - Frequency in Hz (e.g., 440.0 for A4 note)
    /// * `sample_rate` - Sample rate in Hz (e.g., 48000)
    /// * `channels` - Number of channels (1 = mono, 2 = stereo)
    ///
    /// # Example
    ///
    /// ```
    /// use streamlib::TestToneGenerator;
    ///
    /// // 440Hz tone at 48kHz stereo
    /// let gen = TestToneGenerator::new(440.0, 48000, 2);
    /// ```
    pub fn new(frequency: f64, sample_rate: u32, channels: u32) -> Self {
        // Default buffer size: 2048 samples per channel
        // This matches typical audio plugin buffer sizes
        let samples_per_frame = 2048;

        Self {
            frequency,
            sample_rate,
            channels,
            phase: 0.0,
            amplitude: 0.5, // 50% amplitude to avoid clipping
            frame_number: 0,
            samples_per_frame,
            output_port: "audio".to_string(),
        }
    }

    /// Set amplitude (0.0 to 1.0)
    pub fn set_amplitude(&mut self, amplitude: f64) {
        self.amplitude = amplitude.clamp(0.0, 1.0);
    }

    /// Set samples per frame
    ///
    /// Useful for adjusting buffer size based on tick rate
    pub fn set_samples_per_frame(&mut self, samples: usize) {
        self.samples_per_frame = samples;
    }

    /// Generate next audio frame
    pub fn generate_frame(&mut self, timestamp_ns: i64) -> AudioFrame {
        let mut samples = Vec::with_capacity(self.samples_per_frame * self.channels as usize);

        // Phase increment per sample
        let phase_increment = 2.0 * PI * self.frequency / self.sample_rate as f64;

        // Generate samples
        for _ in 0..self.samples_per_frame {
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
    fn descriptor() -> Option<crate::core::schema::ProcessorDescriptor> {
        use crate::core::schema::{ProcessorDescriptor, AudioRequirements};

        Some(
            ProcessorDescriptor::new(
                "TestToneGenerator",
                "Generates sine wave test tones for audio testing and validation"
            )
            .with_usage_context(
                "Use for testing audio output processors without requiring microphone input. \
                 Can generate tones at any frequency and amplitude."
            )
            .with_audio_requirements(AudioRequirements {
                preferred_buffer_size: Some(2048),  // Standard audio plugin buffer size
                required_buffer_size: None,          // But flexible - can adapt
                supported_sample_rates: vec![],      // Any sample rate supported
                required_channels: None,             // Any channel count supported
            })
            .with_tags(vec!["audio", "generator", "test"])
        )
    }

    fn process(&mut self, _tick: TimedTick) -> Result<()> {
        // Note: Actual output to port will be handled by runtime
        // This is a placeholder - the runtime will call generate_frame() separately
        Ok(())
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_tone_generator() {
        let gen = TestToneGenerator::new(440.0, 48000, 2);
        assert_eq!(gen.frequency, 440.0);
        assert_eq!(gen.sample_rate, 48000);
        assert_eq!(gen.channels, 2);
        assert_eq!(gen.phase, 0.0);
        assert_eq!(gen.frame_number, 0);
    }

    #[test]
    fn test_generate_frame() {
        let mut gen = TestToneGenerator::new(440.0, 48000, 2);
        gen.set_samples_per_frame(100); // 100 samples per frame

        let frame = gen.generate_frame(0);

        // Check frame properties
        assert_eq!(frame.sample_count, 100);
        assert_eq!(frame.channels, 2);
        assert_eq!(frame.sample_rate, 48000);
        assert_eq!(frame.frame_number, 0);

        // Check samples array size (100 samples * 2 channels = 200 total)
        assert_eq!(frame.samples.len(), 200);

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
        let mut gen = TestToneGenerator::new(440.0, 48000, 2);

        let frame1 = gen.generate_frame(0);
        assert_eq!(frame1.frame_number, 0);

        let frame2 = gen.generate_frame(10_000_000); // 10ms later
        assert_eq!(frame2.frame_number, 1);

        let frame3 = gen.generate_frame(20_000_000); // 20ms later
        assert_eq!(frame3.frame_number, 2);
    }

    #[test]
    fn test_amplitude_control() {
        let mut gen = TestToneGenerator::new(440.0, 48000, 1);
        gen.set_samples_per_frame(100);

        // Test at 100% amplitude
        gen.set_amplitude(1.0);
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
    fn test_mono_vs_stereo() {
        let mut gen_mono = TestToneGenerator::new(440.0, 48000, 1);
        gen_mono.set_samples_per_frame(100);

        let mut gen_stereo = TestToneGenerator::new(440.0, 48000, 2);
        gen_stereo.set_samples_per_frame(100);

        let frame_mono = gen_mono.generate_frame(0);
        let frame_stereo = gen_stereo.generate_frame(0);

        // Mono should have 100 samples, stereo should have 200
        assert_eq!(frame_mono.samples.len(), 100);
        assert_eq!(frame_stereo.samples.len(), 200);

        // Stereo should have duplicate samples (L, R pairs)
        for i in (0..frame_stereo.samples.len()).step_by(2) {
            assert_eq!(
                frame_stereo.samples[i],
                frame_stereo.samples[i + 1],
                "Stereo L/R channels should be identical for test tone"
            );
        }
    }

    #[test]
    fn test_phase_continuity() {
        let mut gen = TestToneGenerator::new(440.0, 48000, 1);
        gen.set_samples_per_frame(100);

        // Generate multiple frames
        gen.generate_frame(0);
        gen.generate_frame(10_000_000);
        gen.generate_frame(20_000_000);

        // Phase should have advanced but stayed within [0, 2π)
        assert!(gen.phase >= 0.0);
        assert!(gen.phase < 2.0 * PI);
    }
}
