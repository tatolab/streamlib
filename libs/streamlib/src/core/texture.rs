// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Re-exports RHI texture types for convenience.

pub use super::rhi::{StreamTexture, TextureDescriptor, TextureFormat, TextureUsages};

pub mod prelude {
    pub use super::super::rhi::{StreamTexture, TextureDescriptor, TextureFormat, TextureUsages};
}
