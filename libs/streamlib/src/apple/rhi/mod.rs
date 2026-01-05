// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Metal backend implementation for RHI.

pub mod gl_interop_macos;
mod metal_command_buffer;
mod metal_device;
mod metal_texture;

pub use metal_command_buffer::MetalCommandBuffer;
pub use metal_device::MetalDevice;
pub use metal_texture::MetalTexture;
