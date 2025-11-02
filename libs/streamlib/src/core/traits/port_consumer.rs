use super::super::{VideoFrame, AudioFrame};

/// Type-erased port consumer for dynamic port wiring
///
/// This enum allows the runtime to transfer consumers between ports
/// without knowing the specific message type at compile time.
/// Each variant wraps a typed rtrb::Consumer for a specific message type.
pub enum PortConsumer {
    /// Video frame consumer (GPU textures)
    Video(rtrb::Consumer<VideoFrame>),
    /// Audio frame consumer (sample buffers)
    Audio(rtrb::Consumer<AudioFrame>),
    // Future: Add more variants as needed (Data, ML, etc.)
}
