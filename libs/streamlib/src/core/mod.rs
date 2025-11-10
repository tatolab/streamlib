
pub mod bus;
pub mod clap;
pub mod context;
pub mod error;
pub mod frames;
pub mod handles;
pub mod media_clock;
pub mod registry;
pub mod runtime;
pub mod schema;
pub mod scheduling;
pub mod sources;
pub mod sinks;
pub mod transformers;
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
    Bus, ProcessorConnection, ConnectionId, ConnectionManager,
    PortAddress, PortType, PortMessage,
    StreamOutput, StreamInput,
};
pub use context::{GpuContext, AudioContext, RuntimeContext};
pub use error::{StreamError, Result};
pub use runtime::{StreamRuntime, WakeupEvent, ShaderId};
pub use handles::{ProcessorHandle, ProcessorId, OutputPortRef, InputPortRef};
pub use frames::{
    VideoFrame, AudioFrame, DataFrame, MetadataValue,
};
pub use traits::{StreamElement, ElementType, DynStreamElement, StreamProcessor};

pub use sources::{
    CameraProcessor, CameraDevice, CameraConfig,
    AudioCaptureProcessor, AudioInputDevice, AudioCaptureOutputPorts, AudioCaptureConfig,
    ChordGeneratorProcessor, ChordGeneratorOutputPorts, ChordGeneratorConfig,
};
pub use sinks::{
    DisplayProcessor, WindowId, DisplayConfig,
    AudioOutputProcessor, AudioDevice, AudioOutputConfig,
};
pub use transformers::{
    ClapEffectProcessor, ClapScanner, ClapPluginInfo, ClapEffectConfig,
    ClapEffectInputPorts, ClapEffectOutputPorts,
    AudioMixerProcessor, MixingStrategy,
    AudioMixerOutputPorts, AudioMixerConfig,
};

#[cfg(feature = "debug-overlay")]
pub use transformers::{
    PerformanceOverlayProcessor, PerformanceOverlayInputPorts, PerformanceOverlayOutputPorts,
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
