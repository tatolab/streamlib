pub mod clap;
pub mod compat;
pub mod compiler;
pub mod context;
pub mod delegates;
pub mod error;
pub mod execution;

pub mod frames;
pub mod graph;
pub mod links;
pub mod media_clock;
pub mod observability;
pub mod prelude;
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

pub use clap::{
    ClapParameterControl, LfoWaveform, ParameterAutomation, ParameterInfo, ParameterModulator,
    PluginInfo,
};
pub use compiler::{
    compute_delta, compute_delta_with_config, GraphDelta, LinkConfigChange, ProcessorConfigChange,
};
pub use context::{GpuContext, RuntimeContext};
pub use error::{Result, StreamError};

pub use frames::{
    AudioChannelCount, AudioFrame, DataFrame, DynamicFrame, MetadataValue, VideoFrame,
};
pub use graph::{
    compute_config_checksum, input, output, EcsComponentJson, Graph, GraphChecksum,
    InputPortMarker, Link, LinkPortRef, OutputPortMarker, ProcessorId, ProcessorNode,
};
pub use links::{
    LinkId, LinkInput, LinkInputDataReader, LinkInstance, LinkOutput, LinkOutputDataWriter,
    LinkOutputToProcessorMessage, LinkPortAddress, LinkPortMessage, LinkPortType,
    DEFAULT_LINK_CAPACITY,
};
pub use processors::{
    BaseProcessor, BoxedProcessor, CompositeFactory, DynProcessor, EmptyConfig, Processor,
    ProcessorNodeFactory, ProcessorState, ProcessorType, RegistryBackedFactory,
};
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

pub use delegates::{
    FactoryDelegate, LinkDelegate, ProcessorDelegate, SchedulerDelegate, SchedulingStrategy,
    ThreadPriority as SchedulerThreadPriority,
};
pub use execution::{ExecutionConfig, ProcessExecution, ThreadPriority};
pub use registry::{
    global_registry, is_processor_registered, list_processors, list_processors_by_tag,
    register_processor, unregister_processor, DescriptorProvider, ProcessorRegistration,
    ProcessorRegistry,
};
pub use runtime::delegates::{
    DefaultFactory, DefaultLinkDelegate, DefaultProcessorDelegate, DefaultScheduler, FactoryAdapter,
};
pub use runtime::{CommitMode, RuntimeBuilder, StreamRuntime};
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

pub use observability::{
    GraphHealth, GraphInspector, LatencyStats, LinkSnapshot, ProcessorSnapshot,
};
