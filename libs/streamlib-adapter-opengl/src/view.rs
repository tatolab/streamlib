// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Read and write views handed back to consumers inside an acquire scope.
//!
//! The customer-facing payload is intentionally minimal: a
//! `gl_texture_id: u32` plus the GL binding `target`. For host-allocated
//! render-target-capable surfaces the target is `GL_TEXTURE_2D` (the
//! adapter picks a tiled, render-target-capable modifier per the
//! NVIDIA EGL DMA-BUF render-target learning). For sampler-only inputs
//! imported via [`crate::OpenGlSurfaceAdapter::register_external_oes_host_surface`]
//! the target is `GL_TEXTURE_EXTERNAL_OES` and the consumer's GLSL must
//! `#extension GL_OES_EGL_image_external : require` and sample via
//! `texture2D(samplerExternalOES, vec2)` — see
//! [`crate::OpenGlSurfaceAdapter::register_external_oes_host_surface`]
//! for why the unified `texture(...)` overload is not available on the
//! adapter's desktop-GL context.

use std::marker::PhantomData;

use streamlib_adapter_abi::GlWritable;

/// `GL_TEXTURE_2D` enumerant. Re-exported so customers don't need a
/// `gl` crate import to compare `view.target`.
pub const GL_TEXTURE_2D: u32 = 0x0DE1;

/// `GL_TEXTURE_EXTERNAL_OES` enumerant from `GL_OES_EGL_image_external`.
/// Returned by views over surfaces registered through
/// [`crate::OpenGlSurfaceAdapter::register_external_oes_host_surface`] —
/// typically camera frames or other linear / sampler-only DMA-BUFs that
/// NVIDIA's EGL marks `external_only=TRUE`.
pub const GL_TEXTURE_EXTERNAL_OES: u32 = 0x8D65;

/// Read view of an acquired surface.
///
/// `gl_texture_id` is bound to either `GL_TEXTURE_2D` (the default
/// host-render-target path) or `GL_TEXTURE_EXTERNAL_OES` (the
/// sampler-only camera/linear DMA-BUF path); the consumer reads
/// [`Self::target`] to choose the right GLSL sampler.
pub struct OpenGlReadView<'g> {
    pub(crate) texture: u32,
    pub(crate) target: u32,
    pub(crate) _marker: PhantomData<&'g ()>,
}

impl OpenGlReadView<'_> {
    /// The GL texture id bound to the surface's DMA-BUF backing.
    pub fn gl_texture_id(&self) -> u32 {
        self.texture
    }

    /// The GL binding target — `GL_TEXTURE_2D` for host-allocated
    /// render targets, `GL_TEXTURE_EXTERNAL_OES` for surfaces
    /// registered via the external-OES path.
    pub fn target(&self) -> u32 {
        self.target
    }
}

impl GlWritable for OpenGlReadView<'_> {
    fn gl_texture_id(&self) -> u32 {
        self.texture
    }
}

/// Write view of an acquired surface.
///
/// Always backed by a `GL_TEXTURE_2D` — write-side acquires only apply
/// to host-allocated render-target-capable surfaces. The
/// external-OES path is read-only by construction (the underlying
/// import is sampler-only on NVIDIA per
/// `docs/learnings/nvidia-egl-dmabuf-render-target.md`).
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
