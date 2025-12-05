// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

pub use wgpu::{Texture, TextureDescriptor, TextureFormat, TextureUsages, TextureView};

pub mod prelude {
    pub use wgpu::{
        Extent3d, Texture, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
        TextureView,
    };
}
