
pub mod audio_resample_utils;
pub mod bus;
pub mod clap;
pub mod context;
pub mod error;
pub mod loop_utils;
pub mod pubsub;
pub mod signals;
pub mod frames;
pub mod handles;
pub mod media_clock;
pub mod registry;
pub mod runtime;
pub mod schema;
pub mod scheduling;
pub mod processors;
pub mod streaming;
pub mod sync;
pub mod texture;
pub mod topology;
pub mod traits;

pub use clap::{
    ParameterInfo, PluginInfo,
    ParameterModulator, LfoWaveform,
    ParameterAutomation, ClapParameterControl,
};
pub use bus::{
    Bus, ConnectionId, ConnectionManager,
    PortAddress, PortType, PortMessage,
    StreamOutput, StreamInput,
    OwnedProducer, OwnedConsumer, create_owned_connection,
};
pub use context::{GpuContext, RuntimeContext};
pub use error::{StreamError, Result};
pub use pubsub::{
    EVENT_BUS, Event, RuntimeEvent, ProcessorEvent, ProcessorState,
    KeyCode, KeyState, Modifiers,
    MouseButton, MouseState,
    WindowEventType,
    EventListener,
};
pub use runtime::{StreamRuntime, WakeupEvent, ShaderId};
pub use handles::{ProcessorHandle, ProcessorId, OutputPortRef, InputPortRef};
pub use frames::{
    VideoFrame, AudioFrame, DataFrame, MetadataValue,
};
pub use traits::{StreamElement, ElementType, DynStreamElement, StreamProcessor, EmptyConfig};

pub use processors::{
    // Sources
    CameraProcessor, CameraDevice, CameraConfig,
    AudioCaptureProcessor, AudioInputDevice, AudioCaptureConfig,
    ChordGeneratorProcessor, ChordGeneratorConfig,
    // Sinks
    DisplayProcessor, WindowId, DisplayConfig,
    AudioOutputProcessor, AudioDevice, AudioOutputConfig,
    Mp4WriterProcessor, Mp4WriterConfig,
    // Transformers
    ClapEffectProcessor, ClapScanner, ClapPluginInfo, ClapEffectConfig,
    AudioMixerProcessor, MixingStrategy, AudioMixerConfig,
    AudioResamplerProcessor, AudioResamplerConfig, ResamplingQuality,
    AudioChannelConverterProcessor, AudioChannelConverterConfig, ChannelConversionMode,
    BufferRechunkerProcessor, BufferRechunkerConfig,
};

#[cfg(feature = "debug-overlay")]
pub use processors::{
    PerformanceOverlayProcessor,
    PerformanceOverlayConfig,
};
pub use schema::{
    Schema, Field, FieldType, SemanticVersion, SerializationFormat,
    ProcessorDescriptor, PortDescriptor, ProcessorExample,
    AudioRequirements,
    SCHEMA_VIDEO_FRAME, SCHEMA_AUDIO_FRAME, SCHEMA_DATA_MESSAGE,
    SCHEMA_BOUNDING_BOX, SCHEMA_OBJECT_DETECTIONS,
};
pub use sync::{
    timestamp_delta_ms, video_audio_delta_ms,
    are_synchronized, video_audio_synchronized, video_audio_synchronized_with_tolerance,
    DEFAULT_SYNC_TOLERANCE_MS,
};
pub use registry::{
    ProcessorRegistry, ProcessorRegistration,
    DescriptorProvider,
    global_registry,
    register_processor,
    list_processors, list_processors_by_tag,
    is_processor_registered, unregister_processor,
};
pub use texture::{Texture, TextureDescriptor, TextureFormat, TextureUsages, TextureView};
pub use topology::{ConnectionTopology, TopologyAnalyzer, NodeInfo, PortInfo, Edge};
pub use scheduling::{
    SchedulingConfig, SchedulingMode, ThreadPriority,
};
pub use loop_utils::{
    shutdown_aware_loop, LoopControl,
};
pub use streaming::{
    OpusEncoder, AudioEncoderOpus, AudioEncoderConfig, EncodedAudioFrame,
    convert_video_to_samples, convert_audio_to_sample, RtpTimestampCalculator,
};
