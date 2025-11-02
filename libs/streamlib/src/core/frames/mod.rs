//! Frame types for stream data
//!
//! These types define the **data contracts** between processors.
//! All GPU data uses **WebGPU (wgpu)** as the intermediate representation.
//!
//! Platform-specific crates (streamlib-apple, streamlib-linux) convert
//! their native GPU types (Metal, Vulkan) to/from WebGPU internally.
//!
//! This provides:
//! - Zero-copy GPU operations (via wgpu-hal bridges)
//! - Platform-agnostic shader effects
//! - Simple, concrete types (no trait objects)
//!
//! ## Frame Types
//!
//! - **VideoFrame**: GPU-resident video frames (WebGPU textures)
//! - **AudioFrame**: CPU-resident audio samples (with optional GPU buffer)
//! - **DataFrame**: Generic GPU-resident data (WebGPU buffers)
//! - **MetadataValue**: Flexible metadata for annotations

pub mod video_frame;
pub mod audio_frame;
pub mod data_frame;
pub mod metadata;

pub use video_frame::VideoFrame;
pub use audio_frame::AudioFrame;
pub use data_frame::DataFrame;
pub use metadata::MetadataValue;
