// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Read and write views handed to consumers inside an acquire scope.
//!
//! The view owns the Skia `Surface` / `Image` and the inner
//! `VulkanSurfaceAdapter` guard. On drop the view (a) flushes any
//! pending Skia work via `Surface::flush_and_submit` against a
//! `MutexGuard<DirectContext>` taken for the duration of the flush,
//! then (b) releases the inner Vulkan guard, which signals the next
//! timeline value via `inner.end_write_access` / `end_read_access`.
//!
//! This is why [`crate::SkiaSurfaceAdapter::end_write_access`] /
//! [`crate::SkiaSurfaceAdapter::end_read_access`] are no-ops: the
//! field-drop order in `streamlib_adapter_abi::WriteGuard` runs the
//! adapter's `end_*_access` *before* dropping the view, but our work
//! has to happen in the opposite order — flush THEN signal — so the
//! drop logic lives on the view itself.

use std::mem::ManuallyDrop;
use std::sync::{Arc, Mutex};

use streamlib_adapter_abi::{ReadGuard, WriteGuard};
use streamlib_adapter_vulkan::VulkanSurfaceAdapter;
use streamlib_consumer_rhi::VulkanRhiDevice;
use vulkanalia::vk;

use crate::skia_internal::{
    assert_skia_views_not_cpu_readable, drop_skia_image_under_lock,
    flush_and_drop_skia_surface, SyncDirectContext,
};

/// Write view of an acquired Skia surface — exposes the
/// `skia_safe::Surface` the customer draws into. The customer never
/// sees `GrVkImageInfo`, `VkImageLayout`, or the underlying Vulkan
/// handles; those are managed internally by the adapter.
pub struct SkiaWriteView<'g, D: VulkanRhiDevice + 'static> {
    /// `Some` while the view is live; `None` after the drop hook
    /// has consumed it for flush + release.
    skia_surface: Option<skia_safe::Surface>,
    /// Inner Vulkan write guard. Held in `ManuallyDrop` so the
    /// view's drop hook can flush Skia before the inner guard's
    /// drop signals the timeline. See module docs.
    inner_guard: ManuallyDrop<WriteGuard<'g, VulkanSurfaceAdapter<D>>>,
    /// Skia's `DirectContext` (wrapped to claim `Send + Sync` —
    /// adapter-side mutex serializes access). The `Mutex` makes the
    /// adapter `Sync` (the conformance suite's parallel-readers
    /// thread test demands this); customers should still serialize
    /// Skia work per processor in practice.
    direct_context: Arc<Mutex<SyncDirectContext>>,
    /// Held alongside the `Surface` so any post-flush layout
    /// inspection (`get_vk_image_info` / `set_vk_image_layout`) has
    /// the original handle to query. The `Surface` owns its own
    /// refcount on the underlying `GrBackendRenderTarget`, so this
    /// is currently unused at drop — kept for future expansion
    /// (passing the post-flush layout back into the inner adapter
    /// instead of relying on the static `GENERAL` transition the
    /// inner adapter sets).
    #[allow(dead_code)]
    pub(crate) backend_render_target: skia_safe::gpu::BackendRenderTarget,
}

impl<'g, D: VulkanRhiDevice + 'static> SkiaWriteView<'g, D> {
    /// Construct a Skia write view. Called from
    /// [`crate::SkiaSurfaceAdapter::acquire_write`].
    pub(crate) fn new(
        skia_surface: skia_safe::Surface,
        backend_render_target: skia_safe::gpu::BackendRenderTarget,
        inner_guard: WriteGuard<'g, VulkanSurfaceAdapter<D>>,
        direct_context: Arc<Mutex<SyncDirectContext>>,
    ) -> Self {
        Self {
            skia_surface: Some(skia_surface),
            inner_guard: ManuallyDrop::new(inner_guard),
            direct_context,
            backend_render_target,
        }
    }

    /// Borrow the Skia surface for drawing.
    pub fn surface(&self) -> &skia_safe::Surface {
        self.skia_surface
            .as_ref()
            .expect("SkiaWriteView::surface called after drop")
    }

    /// Mutably borrow the Skia surface for drawing.
    pub fn surface_mut(&mut self) -> &mut skia_safe::Surface {
        self.skia_surface
            .as_mut()
            .expect("SkiaWriteView::surface_mut called after drop")
    }
}

impl<'g, D: VulkanRhiDevice + 'static> Drop for SkiaWriteView<'g, D> {
    fn drop(&mut self) {
        if let Some(surface) = self.skia_surface.take() {
            flush_and_drop_skia_surface(&self.direct_context, surface);
        }
        // SAFETY: inner_guard is in ManuallyDrop so this is the one
        // and only place we drop it. Drop fires `inner.end_write_access`
        // which signals the timeline.
        unsafe { ManuallyDrop::drop(&mut self.inner_guard) };
    }
}

impl<'g, D: VulkanRhiDevice + 'static> std::fmt::Debug for SkiaWriteView<'g, D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkiaWriteView")
            .field("active", &self.skia_surface.is_some())
            .finish_non_exhaustive()
    }
}

// SkiaWriteView is auto-derived `!Send + !Sync` because
// `skia_safe::Surface` is `RCHandle<SkSurface>` (a raw pointer wrapper)
// and rust-skia marks it accordingly. We deliberately do NOT add an
// `unsafe impl Send + Sync` — the SurfaceAdapter trait only requires
// the *adapter* to be `Send + Sync`, not the views. Forcing the views
// to Send would let customers move a Skia Surface across threads,
// breaking Skia's single-thread-affinity contract on `GrDirectContext`.
// Each thread that needs Skia work calls `acquire_write` on its own
// thread, gets its own non-Send guard, and drops it on the same thread.

/// Read view of an acquired Skia surface — exposes a `skia::Image`
/// that wraps the host's `VkImage` for sampling. Skia treats the
/// image as immutable; drop releases the inner read guard.
pub struct SkiaReadView<'g, D: VulkanRhiDevice + 'static> {
    skia_image: Option<skia_safe::Image>,
    inner_guard: ManuallyDrop<ReadGuard<'g, VulkanSurfaceAdapter<D>>>,
    /// Held so the `Image`'s `RCHandle` cleanup path (refcount-decrement
    /// → potential GPU command emission) runs under the same mutex
    /// `SkiaWriteView::drop` uses. Skia's `GrDirectContext` is single-
    /// thread-affine; both read and write drops must lock.
    direct_context: Arc<Mutex<SyncDirectContext>>,
}

impl<'g, D: VulkanRhiDevice + 'static> SkiaReadView<'g, D> {
    pub(crate) fn new(
        skia_image: skia_safe::Image,
        inner_guard: ReadGuard<'g, VulkanSurfaceAdapter<D>>,
        direct_context: Arc<Mutex<SyncDirectContext>>,
    ) -> Self {
        Self {
            skia_image: Some(skia_image),
            inner_guard: ManuallyDrop::new(inner_guard),
            direct_context,
        }
    }

    /// Borrow the Skia image (read-only sampling source).
    pub fn image(&self) -> &skia_safe::Image {
        self.skia_image
            .as_ref()
            .expect("SkiaReadView::image called after drop")
    }
}

impl<'g, D: VulkanRhiDevice + 'static> Drop for SkiaReadView<'g, D> {
    fn drop(&mut self) {
        if let Some(image) = self.skia_image.take() {
            drop_skia_image_under_lock(&self.direct_context, image);
        }
        // SAFETY: same as SkiaWriteView::drop.
        unsafe { ManuallyDrop::drop(&mut self.inner_guard) };
    }
}

impl<'g, D: VulkanRhiDevice + 'static> std::fmt::Debug for SkiaReadView<'g, D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkiaReadView")
            .field("active", &self.skia_image.is_some())
            .finish_non_exhaustive()
    }
}

// SkiaReadView is auto-derived `!Send + !Sync` for the same reason as
// SkiaWriteView — Skia's `Image` is `RCHandle<SkImage>`, and forcing
// the view across threads would break Skia's single-thread contract.
// See SkiaWriteView's note above.

// Compile-time invariant: Skia views must NOT impl `CpuReadable` /
// `CpuWritable`. The macro from `skia_internal` stamps out the
// type-level "ambiguous-impl" trick so the GL backend's views and
// any future Skia backend pick up the same invariant for free.
assert_skia_views_not_cpu_readable!(
    _assert_skia_vk_views_not_cpu_readable,
    SkiaReadView<'static, streamlib_consumer_rhi::ConsumerVulkanDevice>,
    SkiaWriteView<'static, streamlib_consumer_rhi::ConsumerVulkanDevice>,
);

/// Helper used by the adapter when we need a `vk::Format` and other
/// raw vulkanalia values without depending on the trait machinery.
#[allow(dead_code)]
pub(crate) const fn vk_format_undefined() -> vk::Format {
    vk::Format::UNDEFINED
}
