// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Re-exports RHI texture types for convenience.

pub use super::rhi::{Texture, TextureDescriptor, TextureFormat, TextureUsages};

pub mod prelude {
    pub use super::super::rhi::{Texture, TextureDescriptor, TextureFormat, TextureUsages};
}
