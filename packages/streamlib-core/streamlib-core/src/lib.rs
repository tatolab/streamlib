//! streamlib-core: Platform-agnostic GPU streaming primitives
//!
//! This crate defines the core traits and types for streamlib's GPU-based
//! real-time video processing system. Platform-specific implementations
//! (Metal, Vulkan) are provided by separate crates.

pub mod buffers;
pub mod clock;
pub mod ports;
pub mod runtime;
pub mod texture;
pub mod topology;

// Re-export core types
pub use buffers::RingBuffer;
pub use clock::{Clock, TimedTick, SoftwareClock, PTPClock, GenlockClock};
pub use ports::{
    StreamOutput, StreamInput, PortType,
    video_output, video_input,
    audio_output, audio_input,
    data_output, data_input,
};
pub use runtime::{StreamRuntime, StreamHandler, ShaderId, OutputPort, InputPort};
pub use texture::{GpuTexture, PixelFormat};
pub use topology::{ConnectionTopology, TopologyAnalyzer, NodeInfo, PortInfo, Edge};

// Re-export anyhow::Result for convenience
pub use anyhow::Result;
