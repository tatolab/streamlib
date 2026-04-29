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

use skia_safe::gpu::SyncCpu;
use streamlib_adapter_abi::{ReadGuard, WriteGuard};
use streamlib_adapter_vulkan::VulkanSurfaceAdapter;
use streamlib_consumer_rhi::VulkanRhiDevice;
use vulkanalia::vk;

use crate::adapter::SyncDirectContext;

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
        // Order matters:
        //  1. Flush + submit Skia surface, sync_cpu = true → wait
        //     for GPU work to drain so the host can host-signal the
        //     timeline below.
        //  2. Drop the Skia surface (releases its DirectContext refs).
        //  3. Drop the inner WriteGuard → triggers
        //     inner.end_write_access → host-signals the next timeline
        //     value, unblocking the next acquire.
        if let Some(mut surface) = self.skia_surface.take() {
            match self.direct_context.lock() {
                Ok(mut ctx_guard) => {
                    // flush_and_submit_surface flushes Skia's pending
                    // commands for `surface` and submits them to the
                    // GPU; SyncCpu::Yes blocks until the GPU is done.
                    ctx_guard
                        .0
                        .flush_and_submit_surface(&mut surface, Some(SyncCpu::Yes));
                }
                Err(_) => {
                    tracing::error!(
                        "SkiaWriteView::drop: direct_context mutex poisoned — \
                         skipping flush, GPU work may not be drained before timeline signal"
                    );
                }
            }
            // Surface is now dropped — releases its DirectContext refcount.
            drop(surface);
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

// SkiaWriteView is Send + Sync: skia_safe::Surface is Send (rust-skia
// marks RCHandle<SkSurface> Send + !Sync); the Mutex<DirectContext>
// covers Sync semantics. Customers are still expected to drive Skia
// from one thread per context; the trait-level Send + Sync is for the
// adapter shape, not for genuinely parallel Skia drawing.
unsafe impl<'g, D: VulkanRhiDevice + 'static> Send for SkiaWriteView<'g, D> {}
unsafe impl<'g, D: VulkanRhiDevice + 'static> Sync for SkiaWriteView<'g, D> {}

/// Read view of an acquired Skia surface — exposes a `skia::Image`
/// that wraps the host's `VkImage` for sampling. Skia treats the
/// image as immutable; drop releases the inner read guard.
pub struct SkiaReadView<'g, D: VulkanRhiDevice + 'static> {
    skia_image: Option<skia_safe::Image>,
    inner_guard: ManuallyDrop<ReadGuard<'g, VulkanSurfaceAdapter<D>>>,
    /// Held so reads don't tear-down the DirectContext while a Skia
    /// `Image` is still pinning a refcount.
    _direct_context: Arc<Mutex<SyncDirectContext>>,
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
            _direct_context: direct_context,
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
        // No flush needed for read — Skia hasn't issued GPU work
        // against this image. Drop the image (releases DirectContext
        // refcount), then drop the inner guard (signals timeline).
        if let Some(image) = self.skia_image.take() {
            drop(image);
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

unsafe impl<'g, D: VulkanRhiDevice + 'static> Send for SkiaReadView<'g, D> {}
unsafe impl<'g, D: VulkanRhiDevice + 'static> Sync for SkiaReadView<'g, D> {}

// Compile-time assertion: Skia views must NOT impl `CpuReadable` /
// `CpuWritable`. Switching to `streamlib-adapter-cpu-readback` is the
// contractual signal for "I want CPU bytes" — see #514. Same trick the
// vulkan and opengl adapters use.
mod _assert_skia_views_not_cpu_readable {
    use super::{SkiaReadView, SkiaWriteView};
    use streamlib_adapter_abi::CpuReadable;
    use streamlib_consumer_rhi::ConsumerVulkanDevice;

    trait AmbiguousIfImpl<A> {
        fn some_item() {}
    }
    impl<T: ?Sized> AmbiguousIfImpl<()> for T {}
    #[allow(dead_code)]
    struct Invalid;
    impl<T: ?Sized + CpuReadable> AmbiguousIfImpl<Invalid> for T {}

    const _: fn() = || {
        let _ = <SkiaReadView<'static, ConsumerVulkanDevice> as AmbiguousIfImpl<_>>::some_item;
        let _ = <SkiaWriteView<'static, ConsumerVulkanDevice> as AmbiguousIfImpl<_>>::some_item;
    };
}

/// Helper used by the adapter when we need a `vk::Format` and other
/// raw vulkanalia values without depending on the trait machinery.
#[allow(dead_code)]
pub(crate) const fn vk_format_undefined() -> vk::Format {
    vk::Format::UNDEFINED
}
