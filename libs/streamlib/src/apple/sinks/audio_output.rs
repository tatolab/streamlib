use crate::core::{
    AudioDevice,
    Result, StreamError,
    ProcessorDescriptor, PortDescriptor,
    StreamInput,
};
use crate::core::frames::StereoSignal;
use crate::core::ports::PortMessage;
use crate::core::traits::{StreamElement, StreamProcessor, ElementType};
use crate::core::scheduling::{SchedulingConfig, SchedulingMode, ClockSource, ThreadPriority};
use crate::core::clocks::AudioClock;
use cpal::Stream;
use cpal::traits::StreamTrait;
use std::sync::Arc;
use parking_lot::Mutex;

pub struct AudioOutputInputPorts {
    pub audio: StreamInput<StereoSignal>,
}

pub struct AppleAudioOutputProcessor {
    device_id: Option<usize>,
    device_name: String,
    device_info: Option<AudioDevice>,

    input_ports: AudioOutputInputPorts,

    stream: Option<Stream>,
    stream_setup_done: bool,

    sample_rate: u32,
    channels: u32,
    buffer_size: usize,

    audio_clock: Arc<AudioClock>,
}

unsafe impl Send for AppleAudioOutputProcessor {}

impl AppleAudioOutputProcessor {
    fn new_internal(device_id: Option<usize>) -> Result<Self> {
        Ok(Self {
            device_id,
            device_name: "Unknown".to_string(),
            device_info: None,
            input_ports: AudioOutputInputPorts {
                audio: StreamInput::new("audio"),
            },
            stream: None,
            stream_setup_done: false,
            sample_rate: 48000,
            channels: 2,
            buffer_size: 512,
            audio_clock: Arc::new(AudioClock::new(48000, "AudioOutput".to_string())),
        })
    }

    pub fn input_ports(&mut self) -> &mut AudioOutputInputPorts {
        &mut self.input_ports
    }
}

// ============================================================
// StreamElement Implementation
// ============================================================

impl StreamElement for AppleAudioOutputProcessor {
    fn name(&self) -> &str {
        "audio_output"
    }

    fn element_type(&self) -> ElementType {
        ElementType::Sink
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <AppleAudioOutputProcessor as StreamProcessor>::descriptor()
    }

    fn input_ports(&self) -> Vec<PortDescriptor> {
        vec![PortDescriptor {
            name: "audio".to_string(),
            schema: StereoSignal::schema(),
            required: true,
            description: "Stereo audio signal to play through speakers".to_string(),
        }]
    }

    fn start(&mut self, ctx: &crate::core::RuntimeContext) -> Result<()> {
        self.buffer_size = ctx.audio.buffer_size;
        tracing::info!("AudioOutput: start() called (Pull mode - buffer_size: {})", self.buffer_size);
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // Drop stream to stop playback
        self.stream = None;
        tracing::info!("AudioOutput {}: Stopped", self.device_name);
        Ok(())
    }

    fn provides_clock(&self) -> Option<Arc<dyn crate::core::clocks::Clock>> {
        Some(self.audio_clock.clone())
    }

    fn as_sink(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_sink_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

// ============================================================
// StreamProcessor Implementation
// ============================================================

impl StreamProcessor for AppleAudioOutputProcessor {
    type Config = crate::core::AudioOutputConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        let device_id = config.device_id.as_ref().and_then(|s| s.parse::<usize>().ok());
        Self::new_internal(device_id)
    }

    fn process(&mut self) -> Result<()> {
        // In Pull mode, process() is called once by runtime after connections are wired
        // This is where we set up the stream and callback

        if self.stream_setup_done {
            // Already set up, nothing to do
            return Ok(());
        }

        tracing::info!("AudioOutput: process() called - setting up stream now that connections are wired");

        let reader = self.input_ports.audio.take_reader()
            .ok_or_else(|| StreamError::Configuration("Input port not connected".into()))?;

        tracing::info!("AudioOutput: Successfully took reader from input port");

        let reader_arc = Arc::new(Mutex::new(reader));
        let reader_for_callback = Arc::clone(&reader_arc);

        let audio_clock = Arc::clone(&self.audio_clock);

        tracing::info!("AudioOutput: Setting up audio output with cpal");

        let setup = crate::apple::audio_utils::setup_audio_output(
            self.device_id,
            self.buffer_size,
            move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {

                audio_clock.increment_samples(data.len() as u64);

                let mut reader = reader_for_callback.lock();

                if let Some(signal) = reader.read_latest() {
                    let num_frames = data.len() / 2;

                    let samples = signal.take_interleaved(num_frames);

                    tracing::debug!("AudioOutput: Got signal with {} samples", samples.len());

                    let copy_len = data.len().min(samples.len());
                    data[..copy_len].copy_from_slice(&samples[..copy_len]);

                    if copy_len < data.len() {
                        data[copy_len..].fill(0.0);
                    }
                } else {
                    tracing::debug!("AudioOutput: No data available, outputting silence");
                    data.fill(0.0);
                }
            }
        )?;

        // Start playback
        tracing::info!("AudioOutput: Starting cpal stream playback");
        setup.stream.play()
            .map_err(|e| StreamError::Configuration(format!("Failed to start stream: {}", e)))?;

        tracing::info!("AudioOutput: cpal stream.play() succeeded");

        // Store stream and device info
        self.stream = Some(setup.stream);
        self.device_name = setup.device_info.name.clone();
        self.device_info = Some(setup.device_info);
        self.sample_rate = setup.sample_rate;
        self.channels = setup.channels;

        // Update audio clock with actual sample rate
        self.audio_clock = Arc::new(AudioClock::new(
            self.sample_rate,
            format!("CoreAudio ({})", self.device_name)
        ));

        self.stream_setup_done = true;

        tracing::info!(
            "AudioOutput {}: Stream setup complete ({}Hz, {} channels, Pull mode)",
            self.device_name,
            self.sample_rate,
            self.channels
        );

        Ok(())
    }

    fn scheduling_config(&self) -> SchedulingConfig {
        SchedulingConfig {
            mode: SchedulingMode::Pull,  // Hardware callback drives execution
            priority: ThreadPriority::RealTime,
            clock: ClockSource::Audio,
            provide_clock: true,
        }
    }

    fn descriptor() -> Option<ProcessorDescriptor> {
        Some(
            ProcessorDescriptor::new(
                "AppleAudioOutputProcessor",
                "Plays audio through speakers/headphones using CoreAudio. Uses Pull mode where hardware callback drives execution.",
            )
            .with_usage_context(
                "Connect audio input port to upstream processor (TestToneGenerator, AudioMixer, etc.). \
                 The CoreAudio callback will pull samples at hardware rate. \
                 Automatically handles sample rate conversion and buffering."
            )
            .with_tags(vec!["audio", "sink", "output", "coreaudio", "pull-mode", "real-time"])
        )
    }

    fn get_input_port_type(&self, port_name: &str) -> Option<crate::core::ports::PortType> {
        match port_name {
            "audio" => Some(self.input_ports.audio.port_type()),
            _ => None,
        }
    }

    fn connect_bus_to_input(&mut self, port_name: &str, bus: Arc<dyn std::any::Any + Send + Sync>) -> bool {
        if port_name == "audio" {
            self.input_ports.audio.connect_bus(bus)
        } else {
            false
        }
    }

    fn connect_reader_to_input(&mut self, port_name: &str, reader: Box<dyn std::any::Any + Send>) -> bool {
        use crate::core::frames::StereoSignal;
        if let Ok(typed_reader) = reader.downcast::<Box<dyn crate::core::bus::BusReader<StereoSignal>>>() {
            if port_name == "audio" {
                self.input_ports.audio.connect_reader(*typed_reader);
                return true;
            }
        }
        false
    }
}
