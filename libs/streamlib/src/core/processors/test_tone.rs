//! Test tone generator processor
//!
//! Generates sine wave test tones for audio testing and validation.
//! Useful for testing audio output without requiring microphone input.

use crate::core::{AudioFrame, Result, StreamProcessor, StreamOutput};
use std::f64::consts::PI;
use std::sync::Arc;
use std::thread::JoinHandle;
use parking_lot::Mutex;

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

    /// Samples per frame (based on tick rate)
    samples_per_frame: usize,

    /// Output ports
    output_ports: TestToneGeneratorOutputPorts,

    /// Wakeup channel for push-based operation (Phase 3)
    /// Background thread sends DataAvailable events at audio rate
    wakeup_tx: Arc<Mutex<Option<crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>>>>,

    /// Background thread handle that mimics hardware timing
    generator_thread: Option<JoinHandle<()>>,

    /// Shutdown signal for background thread
    shutdown_flag: Arc<std::sync::atomic::AtomicBool>,
}

impl TestToneGenerator {
    /// Create new test tone generator
    ///
    /// # Arguments
    ///
    /// * `frequency` - Frequency in Hz (e.g., 440.0 for A4 note)
    /// * `sample_rate` - Sample rate in Hz (e.g., 48000)
    /// * `tick_rate` - Runtime tick rate in Hz (e.g., 60.0 for 60 FPS)
    /// * `amplitude` - Volume (0.0 to 1.0, where 0.5 is 50% volume)
    ///
    /// # Example
    ///
    /// ```
    /// use streamlib::TestToneGenerator;
    ///
    /// // 440Hz tone at 48kHz, 60 FPS runtime, 50% volume
    /// let gen = TestToneGenerator::new(440.0, 48000, 60.0, 0.5);
    /// ```
    pub fn new(frequency: f64, sample_rate: u32, tick_rate: f64, amplitude: f64) -> Self {
        // Calculate samples per frame based on tick rate
        // At 60 FPS: 48000 / 60 = 800 samples per frame
        // This ensures we generate exactly the right amount for real-time processing
        let samples_per_frame = (sample_rate as f64 / tick_rate).ceil() as usize;

        Self {
            frequency,
            sample_rate,
            channels: 2, // Always stereo for compatibility
            phase: 0.0,
            amplitude: amplitude.clamp(0.0, 1.0),
            frame_number: 0,
            samples_per_frame,
            output_ports: TestToneGeneratorOutputPorts {
                audio: StreamOutput::new("audio"),
            },
            wakeup_tx: Arc::new(Mutex::new(None)),
            generator_thread: None,
            shutdown_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
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
        Self::descriptor()
    }

    fn process(&mut self) -> Result<()> {
        // Push-based operation: Background thread generates frames autonomously
        // This method is called on wakeup events but does nothing since
        // the background thread (spawned in on_start) handles frame generation
        Ok(())
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn set_wakeup_channel(&mut self, wakeup_tx: crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>) {
        // Store wakeup channel
        *self.wakeup_tx.lock() = Some(wakeup_tx.clone());

        // Phase 3: Spawn background thread that mimics hardware timing
        // The thread sends DataAvailable events at the rate audio would naturally arrive
        let wakeup_tx_clone = wakeup_tx;
        let shutdown_flag = self.shutdown_flag.clone();
        let samples_per_frame = self.samples_per_frame;
        let sample_rate = self.sample_rate;

        // Calculate sleep duration based on audio chunk size
        // e.g., 800 samples at 48kHz = 16.67ms per chunk (~60 Hz)
        let chunk_duration = std::time::Duration::from_secs_f64(
            samples_per_frame as f64 / sample_rate as f64
        );

        let thread = std::thread::spawn(move || {
            tracing::debug!(
                "TestToneGenerator: Background thread started ({}ms per chunk, ~{} Hz)",
                chunk_duration.as_millis(),
                1000 / chunk_duration.as_millis().max(1)
            );

            while !shutdown_flag.load(std::sync::atomic::Ordering::Relaxed) {
                // Sleep for one audio chunk duration (mimics hardware timing)
                std::thread::sleep(chunk_duration);

                // Send wakeup event (non-blocking)
                if wakeup_tx_clone.send(crate::core::runtime::WakeupEvent::DataAvailable).is_err() {
                    // Channel closed, processor shut down
                    break;
                }
            }

            tracing::debug!("TestToneGenerator: Background thread stopped");
        });

        self.generator_thread = Some(thread);
        tracing::debug!("TestToneGenerator: Push-based wakeup enabled");
    }

    fn on_stop(&mut self) -> Result<()> {
        // Signal shutdown to background thread
        self.shutdown_flag.store(true, std::sync::atomic::Ordering::Relaxed);

        // Wait for thread to finish
        if let Some(thread) = self.generator_thread.take() {
            let _ = thread.join();
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_tone_generator() {
        let gen = TestToneGenerator::new(440.0, 48000, 60.0, 0.5);
        assert_eq!(gen.frequency, 440.0);
        assert_eq!(gen.sample_rate, 48000);
        assert_eq!(gen.channels, 2); // Always stereo
        assert_eq!(gen.amplitude, 0.5);
        assert_eq!(gen.phase, 0.0);
        assert_eq!(gen.frame_number, 0);
        // At 60 FPS: 48000 / 60 = 800 samples per frame
        assert_eq!(gen.samples_per_frame, 800);
    }

    #[test]
    fn test_samples_per_frame_calculation() {
        // Test various tick rates
        let gen_60fps = TestToneGenerator::new(440.0, 48000, 60.0, 0.5);
        assert_eq!(gen_60fps.samples_per_frame, 800); // 48000 / 60

        let gen_30fps = TestToneGenerator::new(440.0, 48000, 30.0, 0.5);
        assert_eq!(gen_30fps.samples_per_frame, 1600); // 48000 / 30

        let gen_120fps = TestToneGenerator::new(440.0, 48000, 120.0, 0.5);
        assert_eq!(gen_120fps.samples_per_frame, 400); // 48000 / 120
    }

    #[test]
    fn test_generate_frame() {
        let mut gen = TestToneGenerator::new(440.0, 48000, 60.0, 0.5);
        let frame = gen.generate_frame(0);

        // At 60 FPS: 800 samples per frame
        assert_eq!(frame.sample_count, 800);
        assert_eq!(frame.channels, 2); // Always stereo
        assert_eq!(frame.sample_rate, 48000);
        assert_eq!(frame.frame_number, 0);

        // Check samples array size (800 samples * 2 channels = 1600 total)
        assert_eq!(frame.samples.len(), 1600);

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
        let mut gen = TestToneGenerator::new(440.0, 48000, 60.0, 0.5);

        let frame1 = gen.generate_frame(0);
        assert_eq!(frame1.frame_number, 0);

        let frame2 = gen.generate_frame(10_000_000); // 10ms later
        assert_eq!(frame2.frame_number, 1);

        let frame3 = gen.generate_frame(20_000_000); // 20ms later
        assert_eq!(frame3.frame_number, 2);
    }

    #[test]
    fn test_amplitude_control() {
        let mut gen = TestToneGenerator::new(440.0, 48000, 60.0, 1.0);

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
        let mut gen = TestToneGenerator::new(440.0, 48000, 60.0, 0.5);
        let frame = gen.generate_frame(0);

        // At 60 FPS: 800 samples * 2 channels = 1600 total
        assert_eq!(frame.samples.len(), 1600);
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
        let mut gen = TestToneGenerator::new(440.0, 48000, 60.0, 0.5);

        // Generate multiple frames
        gen.generate_frame(0);
        gen.generate_frame(10_000_000);
        gen.generate_frame(20_000_000);

        // Phase should have advanced but stayed within [0, 2π)
        assert!(gen.phase >= 0.0);
        assert!(gen.phase < 2.0 * PI);
    }
}
