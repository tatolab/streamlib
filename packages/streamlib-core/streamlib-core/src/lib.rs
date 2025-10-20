//! streamlib-core: Platform-agnostic GPU streaming primitives
//!
//! This crate defines the core traits and types for streamlib's GPU-based
//! real-time video processing system. Platform-specific implementations
//! (Metal, Vulkan) are provided by separate crates.

pub mod graph;
pub mod runtime;
pub mod texture;

pub use runtime::StreamRuntime;
pub use texture::GpuTexture;
