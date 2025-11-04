use crate::core::traits::{StreamElement, StreamProcessor, ElementType};
use crate::core::scheduling::{SchedulingConfig, SchedulingMode, ClockSource, ThreadPriority};
use crate::core::{AudioFrame, Result, StreamOutput, StreamError};
use crate::core::schema::{ProcessorDescriptor, PortDescriptor, AudioRequirements, SCHEMA_AUDIO_FRAME};
use std::f64::consts::PI;
use std::sync::Arc;
use parking_lot::Mutex;
use serde::{Serialize, Deserialize};

#[cfg(target_os = "macos")]
use cpal::Stream;
#[cfg(target_os = "macos")]
use cpal::traits::StreamTrait;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestToneConfig {
    pub frequency: f64,
    pub amplitude: f64,
}

impl Default for TestToneConfig {
    fn default() -> Self {
        Self {
            frequency: 440.0,
            amplitude: 0.5,
        }
    }
}

pub struct TestToneGeneratorOutputPorts {
    pub audio: Arc<StreamOutput<AudioFrame>>,
}

pub struct TestToneGenerator {
    name: String,
    frequency: f64,
    sample_rate: u32,
    channels: u32,
    phase: Arc<Mutex<f64>>,
    amplitude: f64,
    frame_number: Arc<Mutex<u64>>,
    buffer_size: usize,
    output_ports: TestToneGeneratorOutputPorts,

    #[cfg(target_os = "macos")]
    stream: Option<Stream>,
    #[cfg(target_os = "macos")]
    stream_setup_done: bool,
}

unsafe impl Send for TestToneGenerator {}

impl TestToneGenerator {
    pub fn new(frequency: f64, amplitude: f64) -> Self {
        Self {
            name: "test_tone".to_string(),
            frequency,
            sample_rate: 48000,
            channels: 2,
            phase: Arc::new(Mutex::new(0.0)),
            amplitude: amplitude.clamp(0.0, 1.0),
            frame_number: Arc::new(Mutex::new(0)),
            buffer_size: 512,
            output_ports: TestToneGeneratorOutputPorts {
                audio: Arc::new(StreamOutput::new("audio")),
            },

            #[cfg(target_os = "macos")]
            stream: None,
            #[cfg(target_os = "macos")]
            stream_setup_done: false,
        }
    }

    fn timer_rate_hz(&self) -> f64 {
        self.sample_rate as f64 / self.buffer_size as f64
    }

    pub fn output_ports(&mut self) -> &mut TestToneGeneratorOutputPorts {
        &mut self.output_ports
    }

    pub fn set_amplitude(&mut self, amplitude: f64) {
        self.amplitude = amplitude.clamp(0.0, 1.0);
    }

    fn generate_samples(
        phase: &mut f64,
        frequency: f64,
        amplitude: f64,
        sample_rate: u32,
        buffer_size: usize,
        channels: u32,
    ) -> Vec<f32> {
        let mut samples = Vec::with_capacity(buffer_size * channels as usize);
        let phase_increment = 2.0 * PI * frequency / sample_rate as f64;

        for _ in 0..buffer_size {
            let sample = (phase.sin() * amplitude) as f32;

            for _ in 0..channels {
                samples.push(sample);
            }

            *phase += phase_increment;

            if *phase >= 2.0 * PI {
                *phase -= 2.0 * PI;
            }
        }

        samples
    }
}

impl StreamElement for TestToneGenerator {
    fn name(&self) -> &str {
        &self.name
    }

    fn element_type(&self) -> ElementType {
        ElementType::Source
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <TestToneGenerator as StreamProcessor>::descriptor()
    }

    fn output_ports(&self) -> Vec<PortDescriptor> {
        vec![PortDescriptor {
            name: "audio".to_string(),
            schema: SCHEMA_AUDIO_FRAME.clone(),
            required: true,
            description: "Generated sine wave audio output".to_string(),
        }]
    }

    fn start(&mut self, ctx: &crate::core::RuntimeContext) -> Result<()> {
        self.sample_rate = ctx.audio.sample_rate;
        self.buffer_size = ctx.audio.buffer_size;

        tracing::info!(
            "[TestToneGenerator] start() called: {}Hz tone at {}Hz sample rate, {} samples/buffer",
            self.frequency,
            self.sample_rate,
            self.buffer_size
        );

        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            self.stream = None;
        }
        tracing::info!("[TestToneGenerator] Stopped");
        Ok(())
    }

    fn as_source(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_source_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl StreamProcessor for TestToneGenerator {
    type Config = TestToneConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        Ok(Self::new(
            config.frequency,
            config.amplitude,
        ))
    }

    fn process(&mut self) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            if self.stream_setup_done {
                return Ok(());
            }

            tracing::info!("[TestToneGenerator] process() called - setting up CoreAudio stream");

            let frequency = self.frequency;
            let amplitude = self.amplitude;
            let sample_rate = self.sample_rate;
            let buffer_size = self.buffer_size;
            let channels = self.channels;

            let phase = Arc::clone(&self.phase);
            let frame_number = Arc::clone(&self.frame_number);
            let output_port = Arc::clone(&self.output_ports.audio);

            let setup = crate::apple::audio_utils::setup_audio_output(
                None,
                buffer_size,
                move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                    let timestamp_ns = crate::core::media_clock::MediaClock::now().as_nanos() as i64;

                    let samples = {
                        let mut phase = phase.lock();
                        Self::generate_samples(&mut phase, frequency, amplitude, sample_rate, buffer_size, channels)
                    };

                    let frame_num = {
                        let mut frame_num = frame_number.lock();
                        let num = *frame_num;
                        *frame_num += 1;
                        num
                    };

                    let frame = AudioFrame::new(samples, timestamp_ns, frame_num, channels);
                    output_port.write(frame);

                    data.fill(0.0);
                },
            )?;

            setup.stream.play()
                .map_err(|e| StreamError::Configuration(format!("Failed to start stream: {}", e)))?;

            self.stream = Some(setup.stream);
            self.stream_setup_done = true;

            tracing::info!("[TestToneGenerator] CoreAudio stream started - hardware callback active");
            Ok(())
        }

        #[cfg(not(target_os = "macos"))]
        {
            tracing::debug!("TestToneGenerator: process() called, frame {}", *self.frame_number.lock());

            let timestamp_ns = crate::core::media_clock::MediaClock::now().as_nanos() as i64;

            let samples = {
                let mut phase = self.phase.lock();
                Self::generate_samples(&mut phase, self.frequency, self.amplitude, self.sample_rate, self.buffer_size, self.channels)
            };

            let frame_num = {
                let mut frame_num = self.frame_number.lock();
                let num = *frame_num;
                *frame_num += 1;
                num
            };

            let frame = AudioFrame::new(samples, timestamp_ns, frame_num, self.channels);
            self.output_ports.audio.write(frame);
            Ok(())
        }
    }

    fn scheduling_config(&self) -> SchedulingConfig {
        #[cfg(target_os = "macos")]
        {
            SchedulingConfig {
                mode: SchedulingMode::Pull,
                priority: ThreadPriority::RealTime,
                clock: ClockSource::Audio,
                provide_clock: true,
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            SchedulingConfig {
                mode: SchedulingMode::Loop,
                priority: ThreadPriority::RealTime,
                clock: ClockSource::Audio,
                provide_clock: false,
            }
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
                preferred_buffer_size: None,
                required_buffer_size: None,
                supported_sample_rates: vec![],
                required_channels: None,
            })
            .with_tags(vec!["audio", "source", "generator", "test", "real-time"])
        )
    }

    fn take_output_consumer(&mut self, port_name: &str) -> Option<crate::core::traits::PortConsumer> {
        if port_name == "audio" {
            // Extract the consumer from the output port
            self.output_ports.audio
                .consumer_holder()
                .lock()
                .take()
                .map(|consumer| crate::core::traits::PortConsumer::Audio(consumer))
        } else {
            None
        }
    }

    fn set_output_wakeup(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<crate::core::runtime::WakeupEvent>) {
        if port_name == "audio" {
            self.output_ports.audio.set_downstream_wakeup(wakeup_tx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_tone_generator() {
        let gen = TestToneGenerator::new(440.0, 0.5);
        assert_eq!(gen.frequency, 440.0);
        assert_eq!(gen.channels, 2);
        assert_eq!(gen.amplitude, 0.5);
        assert_eq!(gen.phase, 0.0);
        assert_eq!(gen.frame_number, 0);
    }

    #[test]
    fn test_from_config() {
        let config = TestToneConfig {
            frequency: 440.0,
            amplitude: 0.5,
        };
        let gen = <TestToneGenerator as StreamSource>::from_config(config).unwrap();
        assert_eq!(gen.frequency, 440.0);
        assert_eq!(gen.amplitude, 0.5);
    }

    #[test]
    fn test_generate_frame() {
        let mut gen = TestToneGenerator::new(440.0, 0.5);
        gen.sample_rate = 48000;
        gen.buffer_size = 512;

        let frame = gen.generate_frame(0);

        assert_eq!(frame.sample_count, 512);
        assert_eq!(frame.channels, 2);
        assert_eq!(frame.sample_rate, 48000);
        assert_eq!(frame.frame_number, 0);

        assert_eq!(frame.samples.len(), 1024);

        let has_non_zero = frame.samples.iter().any(|&s| s.abs() > 0.0);
        assert!(has_non_zero, "Generated samples should be non-zero");

        for &sample in frame.samples.iter() {
            assert!(
                sample >= -1.0 && sample <= 1.0,
                "Sample {} out of range",
                sample
            );
        }
    }

    #[test]
    fn test_process() {
        let mut gen = TestToneGenerator::new(440.0, 0.5);
        gen.sample_rate = 48000;
        gen.buffer_size = 512;

        // Process should succeed and write to output port
        gen.process().unwrap();

        // Verify we can read the frame from the output port
        let frame = gen.output_ports.audio.read_latest().unwrap();
        assert_eq!(frame.sample_count(), 512);
        assert_eq!(frame.samples.len(), 1024);
    }

    #[test]
    fn test_frame_counter_increments() {
        let mut gen = TestToneGenerator::new(440.0, 0.5);
        gen.sample_rate = 48000;
        gen.buffer_size = 512;

        let frame1 = gen.generate_frame(0);
        assert_eq!(frame1.frame_number, 0);

        let frame2 = gen.generate_frame(10_000_000);
        assert_eq!(frame2.frame_number, 1);

        let frame3 = gen.generate_frame(20_000_000);
        assert_eq!(frame3.frame_number, 2);
    }

    #[test]
    fn test_amplitude_control() {
        let mut gen = TestToneGenerator::new(440.0, 1.0);
        gen.sample_rate = 48000;
        gen.buffer_size = 512;

        let frame_full = gen.generate_frame(0);
        let max_full = frame_full
            .samples
            .iter()
            .map(|s| s.abs())
            .fold(0.0f32, f32::max);

        gen.set_amplitude(0.5);
        gen.phase = 0.0;
        gen.frame_number = 0;
        let frame_half = gen.generate_frame(0);
        let max_half = frame_half
            .samples
            .iter()
            .map(|s| s.abs())
            .fold(0.0f32, f32::max);

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
        let mut gen = TestToneGenerator::new(440.0, 0.5);
        gen.sample_rate = 48000;
        gen.buffer_size = 512;

        let frame = gen.generate_frame(0);

        assert_eq!(frame.samples.len(), 1024);
        assert_eq!(frame.channels, 2);

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
        let mut gen = TestToneGenerator::new(440.0, 0.5);
        gen.sample_rate = 48000;
        gen.buffer_size = 512;

        gen.generate_frame(0);
        gen.generate_frame(10_000_000);
        gen.generate_frame(20_000_000);

        assert!(gen.phase >= 0.0);
        assert!(gen.phase < 2.0 * PI);
    }

    #[test]
    fn test_element_type() {
        let gen = TestToneGenerator::new(440.0, 0.5);
        assert_eq!(gen.element_type(), ElementType::Source);
    }

    #[test]
    fn test_scheduling_config() {
        let mut gen = TestToneGenerator::new(440.0, 0.5);
        gen.sample_rate = 48000;
        gen.buffer_size = 512;

        let sched = gen.scheduling_config();
        assert_eq!(sched.mode, SchedulingMode::Loop);
        assert_eq!(sched.clock, ClockSource::Audio);
        assert_eq!(sched.rate_hz, Some(93.75));
        assert!(!sched.provide_clock);
    }

    #[test]
    fn test_output_ports_descriptor() {
        let gen = TestToneGenerator::new(440.0, 0.5);
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
