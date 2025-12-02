pub mod clap;
pub mod compiler;
pub mod context;
pub mod error;
pub mod execution;
pub mod executor;
pub mod frames;
pub mod graph;
pub mod link_channel;
pub mod media_clock;
pub mod processors;
pub mod pubsub;
pub mod registry;
pub mod runtime;
pub mod schema;
pub mod signals;
pub mod streaming;
pub mod sync;
pub mod texture;
pub mod utils;

// Backwards compatibility - old scheduling module re-exports new execution module
#[deprecated(
    since = "0.2.0",
    note = "Use `execution` module instead of `scheduling`"
)]
pub mod scheduling {
    //! Deprecated: Use [`crate::core::execution`] instead.
    pub use super::execution::ExecutionConfig as SchedulingConfig;
    pub use super::execution::ProcessExecution as SchedulingMode;
    pub use super::execution::ThreadPriority;
}

pub use clap::{
    ClapParameterControl, LfoWaveform, ParameterAutomation, ParameterInfo, ParameterModulator,
    PluginInfo,
};
pub use context::{GpuContext, RuntimeContext};
pub use error::{Result, StreamError};
pub use executor::{compute_delta, ExecutorState, GraphDelta, RuntimeStatus, SimpleExecutor};
pub use frames::{
    AudioChannelCount, AudioFrame, DataFrame, DynamicFrame, MetadataValue, VideoFrame,
};
pub use graph::{
    compute_config_checksum, input, output, Graph, GraphChecksum, InputPortMarker, Link,
    LinkPortRef, OutputPortMarker, ProcessorId, ProcessorNode,
};
pub use link_channel::{
    create_link_channel, LinkChannel, LinkChannelManager, LinkInput, LinkOutput, LinkOwnedConsumer,
    LinkOwnedProducer, LinkPortAddress, LinkPortMessage, LinkPortType,
};
pub use link_channel::{LinkId, ProcessFunctionEvent};
pub use processors::{
    BaseProcessor, BoxedProcessor, CompositeFactory, DynProcessor, EmptyConfig, Processor,
    ProcessorNodeFactory, ProcessorState, ProcessorType, RegistryBackedFactory,
};
#[allow(deprecated)]
pub use pubsub::EVENT_BUS;
pub use pubsub::{
    Event, EventListener, KeyCode, KeyState, Modifiers, MouseButton, MouseState, ProcessorEvent,
    PubSub, RuntimeEvent, WindowEventType, PUBSUB,
};
pub use utils::{
    convert_audio_frame, convert_channels, resample_frame, AudioRechunker, LoopControl,
    ResamplingQuality,
};

pub use processors::{
    AudioCaptureConfig, AudioCaptureProcessor, AudioChannelConverterConfig,
    AudioChannelConverterProcessor, AudioDevice, AudioInputDevice, AudioMixerConfig,
    AudioMixerProcessor, AudioOutputConfig, AudioOutputProcessor, AudioResamplerConfig,
    AudioResamplerProcessor, BufferRechunkerConfig, BufferRechunkerProcessor, CameraConfig,
    CameraDevice, CameraProcessor, ChannelConversionMode, ChordGeneratorConfig,
    ChordGeneratorProcessor, ClapEffectConfig, ClapEffectProcessor, ClapPluginInfo, ClapScanner,
    DisplayConfig, DisplayProcessor, MixingStrategy, Mp4WriterConfig, Mp4WriterProcessor, WindowId,
};

pub use execution::{ExecutionConfig, ProcessExecution, ThreadPriority};
pub use registry::{
    global_registry, is_processor_registered, list_processors, list_processors_by_tag,
    register_processor, unregister_processor, DescriptorProvider, ProcessorRegistration,
    ProcessorRegistry,
};
pub use runtime::{CommitMode, StreamRuntime};
// Backwards compatibility aliases
#[deprecated(since = "0.2.0", note = "Use ExecutionConfig instead")]
pub type SchedulingConfig = ExecutionConfig;
#[deprecated(since = "0.2.0", note = "Use ProcessExecution instead")]
pub type SchedulingMode = ProcessExecution;
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
pub use utils::shutdown_aware_loop;
