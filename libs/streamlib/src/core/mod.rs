pub mod bus;
pub mod clap;
pub mod context;
pub mod error;
pub mod executor;
pub mod frames;
pub mod graph;
pub mod graph_optimizer;
pub mod media_clock;
pub mod utils;
pub mod processors;
pub mod pubsub;
pub mod registry;
pub mod scheduling;
pub mod schema;
pub mod signals;
pub mod streaming;
pub mod sync;
pub mod texture;
pub mod traits;

pub use utils::{
    convert_audio_frame, convert_channels, resample_frame, AudioRechunker, LoopControl,
    ResamplingQuality,
};
pub use bus::{
    create_owned_connection, Bus, ConnectionManager, OwnedConsumer, OwnedProducer, PortAddress,
    PortMessage, PortType, StreamInput, StreamOutput,
};
pub use clap::{
    ClapParameterControl, LfoWaveform, ParameterAutomation, ParameterInfo, ParameterModulator,
    PluginInfo,
};
pub use context::{GpuContext, RuntimeContext};
pub use error::{Result, StreamError};
pub use frames::{
    AudioChannelCount, AudioFrame, DataFrame, DynamicFrame, MetadataValue, VideoFrame,
};
pub use bus::{ConnectionId, WakeupEvent};
pub use executor::{ExecutorState, LegacyExecutor, RuntimeStatus};
pub use graph::{ConnectionEdge, Graph, ProcessorId, ProcessorNode};
pub use graph_optimizer::{
    compute_config_checksum, ExecutionPlan, GraphChecksum, GraphOptimizer, GraphStats,
};
pub use pubsub::{
    Event, EventListener, KeyCode, KeyState, Modifiers, MouseButton, MouseState, ProcessorEvent,
    ProcessorState, RuntimeEvent, WindowEventType, EVENT_BUS,
};
pub use traits::{DynStreamElement, ElementType, EmptyConfig, StreamElement, StreamProcessor};

pub use processors::{
    AudioCaptureConfig,
    AudioCaptureProcessor,
    AudioChannelConverterConfig,
    AudioChannelConverterProcessor,
    AudioDevice,
    AudioInputDevice,
    AudioMixerConfig,
    AudioMixerProcessor,
    AudioOutputConfig,
    AudioOutputProcessor,
    AudioResamplerConfig,
    AudioResamplerProcessor,
    BufferRechunkerConfig,
    BufferRechunkerProcessor,
    CameraConfig,
    CameraDevice,
    // Sources
    CameraProcessor,
    ChannelConversionMode,
    ChordGeneratorConfig,
    ChordGeneratorProcessor,
    ClapEffectConfig,
    // Transformers
    ClapEffectProcessor,
    ClapPluginInfo,
    ClapScanner,
    DisplayConfig,
    // Sinks
    DisplayProcessor,
    MixingStrategy,
    Mp4WriterConfig,
    Mp4WriterProcessor,
    WindowId,
};

pub use utils::shutdown_aware_loop;
#[cfg(feature = "debug-overlay")]
pub use processors::{PerformanceOverlayConfig, PerformanceOverlayProcessor};
pub use registry::{
    global_registry, is_processor_registered, list_processors, list_processors_by_tag,
    register_processor, unregister_processor, DescriptorProvider, ProcessorRegistration,
    ProcessorRegistry,
};
pub use scheduling::{SchedulingConfig, SchedulingMode, ThreadPriority};
pub use schema::{
    AudioRequirements, Field, FieldType, PortDescriptor, ProcessorDescriptor, ProcessorExample,
    Schema, SemanticVersion, SerializationFormat, SCHEMA_AUDIO_FRAME, SCHEMA_BOUNDING_BOX,
    SCHEMA_DATA_MESSAGE, SCHEMA_OBJECT_DETECTIONS, SCHEMA_VIDEO_FRAME,
};
pub use streaming::{
    convert_audio_to_sample, convert_video_to_samples, AudioEncoderConfig, AudioEncoderOpus,
    EncodedAudioFrame, OpusEncoder, RtpTimestampCalculator,
};
pub use sync::{
    are_synchronized, timestamp_delta_ms, video_audio_delta_ms, video_audio_synchronized,
    video_audio_synchronized_with_tolerance, DEFAULT_SYNC_TOLERANCE_MS,
};
pub use texture::{Texture, TextureDescriptor, TextureFormat, TextureUsages, TextureView};
