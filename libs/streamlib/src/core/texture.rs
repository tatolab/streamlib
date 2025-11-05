
pub use wgpu::{Texture, TextureDescriptor, TextureFormat, TextureUsages, TextureView};

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
