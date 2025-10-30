//! streamlib-core: Platform-agnostic GPU streaming primitives
//!
//! This crate defines the core traits and types for streamlib's GPU-based
//! real-time video processing system. Platform-specific implementations
//! (Metal, Vulkan) are provided by separate crates.

pub mod buffers;
pub mod clock;
pub mod error;
pub mod events;
pub mod gpu_context;
pub mod messages;
pub mod registry;
pub mod schema;
pub mod stream_processor;
pub mod ports;
pub mod processors;
pub mod runtime;
pub mod sync;
pub mod texture;
pub mod topology;

// Re-export core types
pub use buffers::RingBuffer;
pub use clock::{Clock, TimedTick, SoftwareClock, PTPClock, GenlockClock};
pub use error::{StreamError, Result};
pub use events::TickBroadcaster;
pub use gpu_context::GpuContext;
pub use messages::{VideoFrame, AudioFrame, AudioFormat, DataMessage, MetadataValue};
pub use stream_processor::StreamProcessor;
pub use ports::{
    StreamOutput, StreamInput, PortType, PortMessage,
};
pub use processors::{
    CameraProcessor, CameraDevice, CameraOutputPorts,
    DisplayProcessor, WindowId, DisplayInputPorts,
    AudioOutputProcessor, AudioDevice, AudioOutputInputPorts,
    AudioCaptureProcessor, AudioInputDevice, AudioCaptureOutputPorts,
    AudioEffectProcessor, ParameterInfo, PluginInfo,
    AudioEffectInputPorts, AudioEffectOutputPorts,
    ClapEffectProcessor, ClapScanner, ClapPluginInfo,
    ParameterModulator, LfoWaveform,
    ParameterAutomation,
    TestToneGenerator, TestToneGeneratorOutputPorts,
    AudioMixerProcessor, MixingStrategy,
    AudioMixerInputPorts, AudioMixerOutputPorts,
};

#[cfg(feature = "debug-overlay")]
pub use processors::{
    PerformanceOverlayProcessor, PerformanceOverlayInputPorts, PerformanceOverlayOutputPorts,
};
pub use runtime::{StreamRuntime, ShaderId};
pub use schema::{
    Schema, Field, FieldType, SemanticVersion, SerializationFormat,
    ProcessorDescriptor, PortDescriptor, ProcessorExample,
    AudioRequirements,  // Audio configuration requirements
    TimerRequirements,  // Timer configuration requirements
    // Standard schemas
    SCHEMA_VIDEO_FRAME, SCHEMA_AUDIO_FRAME, SCHEMA_DATA_MESSAGE,
    SCHEMA_BOUNDING_BOX, SCHEMA_OBJECT_DETECTIONS,
};
pub use sync::{
    timestamp_delta_ms, video_audio_delta_ms,
    are_synchronized, video_audio_synchronized, video_audio_synchronized_with_tolerance,
    MultimodalBuffer, DEFAULT_SYNC_TOLERANCE_MS,
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
