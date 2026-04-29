// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Read and write views for the GL-backed Skia adapter.
//!
//! Symmetric with the Vulkan-backed `SkiaWriteView` / `SkiaReadView`:
//! the customer sees only `skia_safe::Surface` (write) or
//! `skia_safe::Image` (read); the GL `texture id` and the underlying
//! DMA-BUF FD never reach them.
//!
//! The drop ordering is the GL flavor of the same recipe used by the
//! Vulkan side. Skia work must run on a thread with the EGL context
//! current, so the view's drop hook locks the
//! [`EglRuntime::lock_make_current`][lmc] guard *before* it locks
//! `direct_context`. After Skia's flush, the EGL lock is released
//! before the inner [`OpenGlSurfaceAdapter`]'s release path runs —
//! the inner adapter's `end_*_access` re-acquires `lock_make_current`
//! to issue `glFinish`, and holding both at once would deadlock.
//!
//! [lmc]: streamlib_adapter_opengl::EglRuntime::lock_make_current

use std::mem::ManuallyDrop;
use std::sync::{Arc, Mutex};

use streamlib_adapter_abi::{ReadGuard, WriteGuard};
use streamlib_adapter_opengl::{EglRuntime, OpenGlSurfaceAdapter};

use crate::skia_internal::{
    assert_skia_views_not_cpu_readable, drop_skia_image_under_lock,
    flush_and_drop_skia_surface, SyncDirectContext,
};

/// Write view for the GL-backed Skia adapter.
pub struct SkiaGlWriteView<'g> {
    /// `Some` while the view is live; `None` after the drop hook has
    /// consumed it for flush + release.
    skia_surface: Option<skia_safe::Surface>,
    /// Inner GL write guard. Wrapped in `ManuallyDrop` so the view's
    /// drop hook can flush Skia + release the EGL lock before the
    /// inner adapter's `end_write_access` runs.
    inner_guard: ManuallyDrop<WriteGuard<'g, OpenGlSurfaceAdapter>>,
    direct_context: Arc<Mutex<SyncDirectContext>>,
    egl: Arc<EglRuntime>,
    /// Held alongside the `Surface` to keep the imported `BackendTexture`
    /// alive across the surface's lifetime. Skia's `Surface` does not
    /// own its backing texture by refcount on the GL side — the
    /// `BackendTexture` value owns the wrap-state, and dropping it
    /// before the surface is invalid.
    #[allow(dead_code)]
    pub(crate) backend_texture: skia_safe::gpu::BackendTexture,
}

impl<'g> SkiaGlWriteView<'g> {
    pub(crate) fn new(
        skia_surface: skia_safe::Surface,
        backend_texture: skia_safe::gpu::BackendTexture,
        inner_guard: WriteGuard<'g, OpenGlSurfaceAdapter>,
        direct_context: Arc<Mutex<SyncDirectContext>>,
        egl: Arc<EglRuntime>,
    ) -> Self {
        Self {
            skia_surface: Some(skia_surface),
            inner_guard: ManuallyDrop::new(inner_guard),
            direct_context,
            egl,
            backend_texture,
        }
    }

    /// Borrow the Skia surface for drawing.
    pub fn surface(&self) -> &skia_safe::Surface {
        self.skia_surface
            .as_ref()
            .expect("SkiaGlWriteView::surface called after drop")
    }

    /// Mutably borrow the Skia surface for drawing.
    pub fn surface_mut(&mut self) -> &mut skia_safe::Surface {
        self.skia_surface
            .as_mut()
            .expect("SkiaGlWriteView::surface_mut called after drop")
    }
}

impl<'g> Drop for SkiaGlWriteView<'g> {
    fn drop(&mut self) {
        // Order matters:
        //  1. lock_make_current — Skia's flush_and_submit_surface
        //     issues GL commands; the EGL context must be current on
        //     this thread.
        //  2. flush_and_drop_skia_surface — locks `direct_context`,
        //     drains the GPU via SyncCpu::Yes, drops the surface
        //     under the lock.
        //  3. Drop the EGL guard. The inner OpenGl adapter's
        //     `end_write_access` re-acquires `lock_make_current` to
        //     issue `glFinish` — holding the EGL lock while dropping
        //     the inner guard would deadlock (parking_lot's
        //     `make_current_lock` is not reentrant).
        //  4. Drop the inner guard. Triggers
        //     `OpenGlSurfaceAdapter::end_write_access` → glFinish,
        //     which is what the next consumer waits on for handoff.
        if let Some(surface) = self.skia_surface.take() {
            match self.egl.lock_make_current() {
                Ok(_current) => {
                    flush_and_drop_skia_surface(&self.direct_context, surface);
                    // _current drops here, releasing the EGL mutex
                    // before the inner guard's drop runs.
                }
                Err(e) => {
                    tracing::error!(
                        ?e,
                        "SkiaGlWriteView::drop: lock_make_current failed — \
                         skipping flush, GPU work may not be drained before \
                         glFinish"
                    );
                    // Drop the surface anyway so we don't leak. Skia's
                    // internal cleanup may emit GL commands without a
                    // current context; this is a degraded path.
                    drop(surface);
                }
            }
        }

        // SAFETY: `inner_guard` is in `ManuallyDrop` so this is the
        // one and only place we drop it.
        unsafe { ManuallyDrop::drop(&mut self.inner_guard) };
    }
}

impl<'g> std::fmt::Debug for SkiaGlWriteView<'g> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkiaGlWriteView")
            .field("active", &self.skia_surface.is_some())
            .finish_non_exhaustive()
    }
}

// SkiaGlWriteView is auto-derived `!Send + !Sync` for the same reason
// as SkiaWriteView: `skia_safe::Surface` is `RCHandle<SkSurface>`, and
// forcing the view across threads breaks Skia's single-thread
// affinity contract on `GrDirectContext`. See `view.rs`'s note.

/// Read view for the GL-backed Skia adapter.
pub struct SkiaGlReadView<'g> {
    skia_image: Option<skia_safe::Image>,
    inner_guard: ManuallyDrop<ReadGuard<'g, OpenGlSurfaceAdapter>>,
    direct_context: Arc<Mutex<SyncDirectContext>>,
    egl: Arc<EglRuntime>,
}

impl<'g> SkiaGlReadView<'g> {
    pub(crate) fn new(
        skia_image: skia_safe::Image,
        inner_guard: ReadGuard<'g, OpenGlSurfaceAdapter>,
        direct_context: Arc<Mutex<SyncDirectContext>>,
        egl: Arc<EglRuntime>,
    ) -> Self {
        Self {
            skia_image: Some(skia_image),
            inner_guard: ManuallyDrop::new(inner_guard),
            direct_context,
            egl,
        }
    }

    /// Borrow the Skia image (read-only sampling source).
    pub fn image(&self) -> &skia_safe::Image {
        self.skia_image
            .as_ref()
            .expect("SkiaGlReadView::image called after drop")
    }
}

impl<'g> Drop for SkiaGlReadView<'g> {
    fn drop(&mut self) {
        // Reads emit no GPU work, but Skia's `Image::drop` decrements
        // a refcount on `DirectContext` and may issue cleanup commands
        // — same EGL-current invariant as writes.
        if let Some(image) = self.skia_image.take() {
            match self.egl.lock_make_current() {
                Ok(_current) => {
                    drop_skia_image_under_lock(&self.direct_context, image);
                }
                Err(e) => {
                    tracing::error!(
                        ?e,
                        "SkiaGlReadView::drop: lock_make_current failed — \
                         dropping image without an EGL-current context"
                    );
                    drop(image);
                }
            }
        }
        // SAFETY: `inner_guard` is in `ManuallyDrop` so this is the
        // one and only place we drop it.
        unsafe { ManuallyDrop::drop(&mut self.inner_guard) };
    }
}

impl<'g> std::fmt::Debug for SkiaGlReadView<'g> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkiaGlReadView")
            .field("active", &self.skia_image.is_some())
            .finish_non_exhaustive()
    }
}

assert_skia_views_not_cpu_readable!(
    _assert_skia_gl_views_not_cpu_readable,
    SkiaGlReadView<'static>,
    SkiaGlWriteView<'static>,
);
