//! streamlib-core: Platform-agnostic GPU streaming primitives
//!
//! This crate defines the core traits and types for streamlib's GPU-based
//! real-time video processing system. Platform-specific implementations
//! (Metal, Vulkan) are provided by separate crates.

pub mod buffers;
pub mod clock;
pub mod error;
pub mod events;
pub mod messages;
pub mod stream_processor;
pub mod ports;
pub mod runtime;
pub mod texture;
pub mod topology;

// Re-export core types
pub use buffers::RingBuffer;
pub use clock::{Clock, TimedTick, SoftwareClock, PTPClock, GenlockClock};
pub use error::{StreamError, Result};
pub use events::TickBroadcaster;
pub use messages::{VideoFrame, AudioBuffer, DataMessage, GpuData, MetadataValue};
pub use stream_processor::StreamProcessor;
pub use ports::{
    StreamOutput, StreamInput, PortType, PortMessage,
};
pub use runtime::{StreamRuntime, ShaderId};
pub use texture::{GpuTexture, GpuTextureHandle, PixelFormat};
pub use topology::{ConnectionTopology, TopologyAnalyzer, NodeInfo, PortInfo, Edge};
