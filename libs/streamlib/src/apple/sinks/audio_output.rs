use crate::core::{
    AudioDevice,
    Result, StreamError,
    ProcessorDescriptor, PortDescriptor,
    StreamInput,
};
use crate::core::frames::AudioFrame;
use crate::core::bus::PortMessage;
use crate::core::traits::{StreamElement, StreamProcessor, ElementType};
use crate::core::scheduling::{SchedulingConfig, SchedulingMode, ThreadPriority};
use cpal::Stream;
use cpal::traits::StreamTrait;

pub struct AudioOutputInputPorts {
    pub audio: StreamInput<AudioFrame<2>>,
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
        })
    }

    pub fn input_ports(&mut self) -> &mut AudioOutputInputPorts {
        &mut self.input_ports
    }
}


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
            schema: AudioFrame::<2>::schema(),
            required: true,
            description: "Stereo audio frame to play through speakers".to_string(),
        }]
    }

    fn start(&mut self, ctx: &crate::core::RuntimeContext) -> Result<()> {
        self.buffer_size = ctx.audio.buffer_size;
        tracing::info!("AudioOutput: start() called (Pull mode - buffer_size: {})", self.buffer_size);
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.stream = None;
        tracing::info!("AudioOutput {}: Stopped", self.device_name);
        Ok(())
    }

    fn as_sink(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_sink_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}


impl StreamProcessor for AppleAudioOutputProcessor {
    type Config = crate::core::AudioOutputConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        let device_id = config.device_id.as_ref().and_then(|s| s.parse::<usize>().ok());
        Self::new_internal(device_id)
    }

    fn process(&mut self) -> Result<()> {

        if self.stream_setup_done {
            return Ok(());
        }

        tracing::info!("AudioOutput: process() called - setting up stream now that connections are wired");

        let input_connection = self.input_ports.audio.clone_connection()
            .ok_or_else(|| StreamError::Configuration("Input port not connected".into()))?;

        tracing::info!("AudioOutput: Successfully cloned connection from input port");

        let connection_for_callback = input_connection;

        tracing::info!("AudioOutput: Setting up audio output with cpal");

        let setup = crate::apple::audio_utils::setup_audio_output(
            self.device_id,
            self.buffer_size,
            move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                if let Some(audio_frame) = connection_for_callback.read_latest() {
                    let samples = &audio_frame.samples;

                    tracing::debug!("AudioOutput: Got audio frame with {} samples", samples.len());

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

        tracing::info!("AudioOutput: Starting cpal stream playback");
        setup.stream.play()
            .map_err(|e| StreamError::Configuration(format!("Failed to start stream: {}", e)))?;

        tracing::info!("AudioOutput: cpal stream.play() succeeded");

        self.stream = Some(setup.stream);
        self.device_name = setup.device_info.name.clone();
        self.device_info = Some(setup.device_info);
        self.sample_rate = setup.sample_rate;
        self.channels = setup.channels;
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

    fn get_input_port_type(&self, port_name: &str) -> Option<crate::core::bus::PortType> {
        match port_name {
            "audio" => Some(self.input_ports.audio.port_type()),
            _ => None,
        }
    }

    fn wire_input_connection(&mut self, port_name: &str, connection: std::sync::Arc<dyn std::any::Any + Send + Sync>) -> bool {
        use crate::core::bus::ProcessorConnection;
        use crate::core::AudioFrame;

        if let Ok(typed_conn) = connection.downcast::<std::sync::Arc<ProcessorConnection<AudioFrame<2>>>>() {
            if port_name == "audio" {
                self.input_ports.audio.set_connection(std::sync::Arc::clone(&typed_conn));
                return true;
            }
        }
        false
    }
}
