// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Read and write views handed back to consumers inside an acquire scope.
//!
//! The customer-facing payload is intentionally minimal: a
//! `gl_texture_id: u32` and the constant target `GL_TEXTURE_2D`. The
//! NVIDIA `external_only=TRUE` quirk for linear DMA-BUFs (see
//! `docs/learnings/nvidia-egl-dmabuf-render-target.md`) is absorbed by
//! the host allocator picking a tiled, render-target-capable modifier;
//! the customer never types the words "modifier" or "external_only."

use std::marker::PhantomData;

use streamlib_adapter_abi::GlWritable;

/// `GL_TEXTURE_2D` enumerant. Re-exported so customers don't need a
/// `gl` crate import to compare `view.target`.
pub const GL_TEXTURE_2D: u32 = 0x0DE1;

/// Read view of an acquired surface.
///
/// `gl_texture_id` is bound to a render-target-capable
/// `GL_TEXTURE_2D`; the customer can sample from it in a fragment
/// shader (`sampler2D`) or attach it as an FBO color attachment.
pub struct OpenGlReadView<'g> {
    pub(crate) texture: u32,
    pub(crate) _marker: PhantomData<&'g ()>,
}

impl OpenGlReadView<'_> {
    /// The GL texture id bound to the surface's DMA-BUF backing.
    pub fn gl_texture_id(&self) -> u32 {
        self.texture
    }

    /// `GL_TEXTURE_2D` — never `GL_TEXTURE_RECTANGLE` or
    /// `GL_TEXTURE_EXTERNAL_OES`. The host allocator's modifier
    /// choice ensures the import lands as a regular 2D texture.
    pub fn target(&self) -> u32 {
        GL_TEXTURE_2D
    }
}

impl GlWritable for OpenGlReadView<'_> {
    fn gl_texture_id(&self) -> u32 {
        self.texture
    }
}

/// Write view of an acquired surface.
///
/// Identical shape to [`OpenGlReadView`] — both expose a
/// `GL_TEXTURE_2D` id. Distinguished only at the type level so the
/// trait's typestate keeps "I have a read guard but tried to write" a
/// compile error instead of a runtime one.
pub struct OpenGlWriteView<'g> {
    pub(crate) texture: u32,
    pub(crate) _marker: PhantomData<&'g ()>,
}

impl OpenGlWriteView<'_> {
    pub fn gl_texture_id(&self) -> u32 {
        self.texture
    }

    pub fn target(&self) -> u32 {
        GL_TEXTURE_2D
    }
}

impl GlWritable for OpenGlWriteView<'_> {
    fn gl_texture_id(&self) -> u32 {
        self.texture
    }
}
