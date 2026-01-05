// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Render Hardware Interface (RHI) - Platform-agnostic GPU abstraction.

mod command_buffer;
mod device;
mod gl_interop;
mod texture;

pub use command_buffer::CommandBuffer;
pub use device::GpuDevice;
pub use gl_interop::{gl_constants, GlContext, GlTextureBinding};
pub use texture::{
    NativeTextureHandle, StreamTexture, TextureDescriptor, TextureFormat, TextureUsages,
};
