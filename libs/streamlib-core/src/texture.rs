//! GPU texture abstraction using WebGPU
//!
//! streamlib-core uses WebGPU (wgpu) as the universal GPU abstraction.
//! Platform-specific layers (streamlib-apple, streamlib-jetson, etc.)
//! are responsible for bridging native GPU resources to WebGPU textures.

pub use wgpu::{Texture, TextureDescriptor, TextureFormat, TextureUsages, TextureView};

/// Convenience re-export of commonly used WebGPU types for textures
pub mod prelude {
    pub use wgpu::{
        Texture,
        TextureDescriptor,
        TextureFormat,
        TextureUsages,
        TextureView,
        TextureDimension,
        Extent3d,
    };
}
