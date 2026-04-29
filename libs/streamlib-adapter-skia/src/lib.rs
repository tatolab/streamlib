// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Skia surface adapter — composes on `streamlib-adapter-vulkan` so
//! customers get a `skia::Surface` (write) or `skia::Image` (read)
//! straight from a `StreamlibSurface`. The adapter does the
//! `VkImageLayout` transitions, builds the `GrVkBackendContext` once
//! per context, and bridges Skia's `GrFlushInfo` semaphore in/out to
//! the inner adapter's timeline-semaphore signal/wait — none of which
//! the customer ever sees.
//!
//! Trait-composition shape:
//!
//! ```ignore
//! impl<D: VulkanRhiDevice + 'static> SurfaceAdapter for SkiaSurfaceAdapter<D> {
//!     type WriteView<'g> = SkiaWriteView<'g, D>;
//!     type ReadView<'g>  = SkiaReadView<'g, D>;
//! }
//! ```
//!
//! The inner `VulkanWriteView<'g>` is held *inside* `SkiaWriteView`
//! and never reaches the customer; the deliberate
//! `VulkanWritable::vk_image_layout()` escape hatch from
//! `streamlib-adapter-abi` lives on a capability trait that public
//! consumers of `SurfaceAdapter` cannot reach. See
//! `docs/architecture/surface-adapter.md` for the full architecture
//! brief.

#![cfg(target_os = "linux")]

mod adapter;
mod context;
mod error;
mod gl_adapter;
mod gl_context;
mod gl_view;
mod skia_internal;
mod view;

pub use adapter::SkiaSurfaceAdapter;
pub use context::SkiaContext;
pub use error::SkiaAdapterError;
pub use gl_adapter::SkiaGlSurfaceAdapter;
pub use gl_context::SkiaGlContext;
pub use gl_view::{SkiaGlReadView, SkiaGlWriteView};
pub use view::{SkiaReadView, SkiaWriteView};
