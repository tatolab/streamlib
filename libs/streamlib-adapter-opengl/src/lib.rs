// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! OpenGL/EGL surface adapter — host-allocated `VkImage` consumed as a
//! render-target-capable `GL_TEXTURE_2D`.
//!
//! The adapter imports a DMA-BUF FD with the host-chosen DRM modifier
//! via `EGL_EXT_image_dma_buf_import_modifiers` and binds the resulting
//! `EGLImage` via `glEGLImageTargetTexture2DOES`. The customer sees only
//! a `gl_texture_id: u32` and uses it as both a sampler and an FBO color
//! attachment — the NVIDIA `external_only=TRUE` quirk for linear
//! DMA-BUFs (see `docs/learnings/nvidia-egl-dmabuf-render-target.md`)
//! never reaches them because the host allocator already picked a
//! tiled, render-target-capable modifier.
//!
//! **Y-axis convention** — the imported `GL_TEXTURE_2D` is backed by a
//! `VkImage` that downstream Vulkan consumers (display, encoders) read
//! with origin at top-left, while GL writes to an FBO with origin at
//! bottom-left. Producers MUST render Vulkan-conventional or the
//! consumer sees an upside-down image (#621). See [`OpenGlWriteView`]
//! for the full convention and the `pose_overlay_renderer.py` example
//! for an in-tree reference.
//!
//! See `docs/architecture/surface-adapter.md` for the architecture brief
//! and `docs/architecture/adapter-authoring.md` for the 3rd-party
//! authoring guide.

#![cfg(target_os = "linux")]

mod adapter;
mod context;
mod egl;
mod state;
mod view;

pub use adapter::OpenGlSurfaceAdapter;
pub use context::OpenGlContext;
pub use egl::{
    EglRuntime, EglRuntimeError, OwnedMakeCurrentGuard, DRM_FORMAT_ABGR8888,
    DRM_FORMAT_ARGB8888,
};
pub use state::HostSurfaceRegistration;
pub use view::{OpenGlReadView, OpenGlWriteView, GL_TEXTURE_2D, GL_TEXTURE_EXTERNAL_OES};
