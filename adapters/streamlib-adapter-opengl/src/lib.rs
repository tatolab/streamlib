// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

#![cfg(target_os = "linux")]

mod adapter;
mod context;
mod egl;
mod state;
mod view;

pub use adapter::OpenGlSurfaceAdapter;
pub use context::OpenGlContext;
pub use egl::{
    DRM_FORMAT_ABGR8888, DRM_FORMAT_ARGB8888, EglRuntime, EglRuntimeError, OwnedMakeCurrentGuard,
};
pub use state::HostSurfaceRegistration;
pub use view::{GL_TEXTURE_2D, GL_TEXTURE_EXTERNAL_OES, OpenGlReadView, OpenGlWriteView};
