//! streamlib-core: Platform-agnostic GPU streaming primitives
//!
//! This crate defines the core traits and types for streamlib's GPU-based
//! real-time video processing system. Platform-specific implementations
//! (Metal, Vulkan) are provided by separate crates.

pub mod bus;
pub mod clap;
pub mod clocks;
pub mod context;
pub mod error;
pub mod frames;
pub mod handles;
pub mod media_clock;
pub mod registry;
pub mod runtime;
pub mod schema;
pub mod scheduling;
pub mod ports;
pub mod sources;
pub mod sinks;
pub mod transformers;
pub mod sync;
pub mod texture;
pub mod topology;
pub mod traits;

// Re-export core types
pub use clap::{
    ParameterInfo, PluginInfo,
    ParameterModulator, LfoWaveform,
    ParameterAutomation, ClapParameterControl,
};
pub use clocks::{Clock, SoftwareClock, AudioClock, VideoClock, PTPClock, GenlockClock};
pub use context::{GpuContext, AudioContext, RuntimeContext};
pub use error::{StreamError, Result};
pub use runtime::{StreamRuntime, WakeupEvent, ShaderId};
pub use handles::{ProcessorHandle, ProcessorId, OutputPortRef, InputPortRef};
pub use frames::{
    VideoFrame, AudioFrame, DataFrame, MetadataValue,
    MonoSignal, StereoSignal, QuadSignal, FiveOneSignal,
};
// v2.0 traits - GStreamer-inspired hierarchy
pub use traits::{StreamElement, ElementType, DynStreamElement, StreamProcessor};
pub use ports::{
    StreamOutput, StreamInput, PortType, PortMessage,
};

// Re-export processor types and their configs from sources/sinks/transformers
pub use sources::{
    CameraProcessor, CameraDevice, CameraOutputPorts, CameraConfig,
    AudioCaptureProcessor, AudioInputDevice, AudioCaptureOutputPorts, AudioCaptureConfig,
    ChordGeneratorProcessor, ChordGeneratorOutputPorts, ChordGeneratorConfig,
};
pub use sinks::{
    DisplayProcessor, WindowId, DisplayInputPorts, DisplayConfig,
    AudioOutputProcessor, AudioDevice, AudioOutputInputPorts, AudioOutputConfig,
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
    ClockSource, ClockConfig, ClockType, SyncMode,
};
