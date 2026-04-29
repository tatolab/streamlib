// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared scaffolding used by every Skia backend (Vulkan, GL, …).
//!
//! Skia's `GrDirectContext` is single-thread-affine and `!Send` regardless
//! of which graphics backend it sits on top of, and the adapter trait
//! requires `Send + Sync` on the adapter type. The fix is the same on
//! every backend: wrap the `DirectContext` in a newtype that manually
//! claims `Send + Sync`, then serialize every operation through a
//! surrounding `Mutex`. The mutex is what *actually* upholds the
//! single-thread-affinity invariant — the `unsafe impl` is sound iff
//! the mutex is always held during Skia work.
//!
//! The same applies to the surface flush / drop hazard: every backend's
//! `WriteView::drop` must (1) flush + submit the Skia surface (2) drop
//! the Skia surface inside the mutex scope (RCHandle drop emits cleanup
//! commands that must stay on the same thread) (3) release the mutex
//! before triggering the inner adapter's release path. This module
//! exposes a helper that bottles that recipe so each backend's view
//! drop only has to call one function instead of recreating the
//! ordering by hand.

use std::sync::Mutex;

use skia_safe::gpu::{DirectContext, SyncCpu};

/// Newtype wrapping Skia's `DirectContext` to manually claim
/// `Send + Sync`.
///
/// Skia treats `GrDirectContext` as single-thread-affine; the surrounding
/// [`Mutex`] in every backend adapter is what actually upholds that
/// invariant. The `unsafe impl Send + Sync` here is sound *only* when
/// the holder serializes every dereference through that mutex — which
/// every Skia adapter in this crate does.
pub(crate) struct SyncDirectContext(pub(crate) DirectContext);

// SAFETY: Skia's `GrDirectContext` is `!Send + !Sync` because it's
// single-thread-affine. Adapters wrap this newtype in `Mutex<…>` and
// only ever touch the inner `DirectContext` while holding the mutex,
// so the upgrade to `Send + Sync` is sound.
unsafe impl Send for SyncDirectContext {}
unsafe impl Sync for SyncDirectContext {}

/// Flush + drop a Skia surface under the adapter's `DirectContext`
/// mutex.
///
/// Order:
///   1. Acquire the mutex.
///   2. `flush_and_submit_surface(SyncCpu::Yes)` so the GPU drains
///      before the inner adapter's release-side timeline signal /
///      `glFinish` fires.
///   3. Drop the surface *inside* the mutex scope — `RCHandle::drop`
///      decrements the surface's refcount on the `DirectContext` and
///      can issue cleanup commands. Skia's `DirectContext` is
///      single-thread-affine; doing the drop after the mutex is
///      released runs RCHandle cleanup unprotected.
///
/// On poisoned-mutex the surface is dropped without a flush — degraded
/// path, no panic. Logged at error level so a downstream watchdog can
/// flag the loss of the GPU drain.
pub(crate) fn flush_and_drop_skia_surface(
    direct_context: &Mutex<SyncDirectContext>,
    mut surface: skia_safe::Surface,
) {
    match direct_context.lock() {
        Ok(mut ctx_guard) => {
            ctx_guard
                .0
                .flush_and_submit_surface(&mut surface, Some(SyncCpu::Yes));
            drop(surface);
        }
        Err(_) => {
            tracing::error!(
                "skia_internal::flush_and_drop_skia_surface: direct_context mutex \
                 poisoned — skipping flush, GPU work may not be drained before \
                 the inner adapter's release signal"
            );
            drop(surface);
        }
    }
}

/// Drop a Skia image under the adapter's `DirectContext` mutex.
///
/// Read views don't need a flush (Skia hasn't issued GPU work against
/// the image — `borrow_texture_from` just wraps a sampler-source
/// view), but the `Image`'s `RCHandle::drop` still emits cleanup on
/// the `DirectContext` and must run while the mutex is held.
pub(crate) fn drop_skia_image_under_lock(
    direct_context: &Mutex<SyncDirectContext>,
    image: skia_safe::Image,
) {
    match direct_context.lock() {
        Ok(_ctx_guard) => drop(image),
        Err(_) => {
            tracing::error!(
                "skia_internal::drop_skia_image_under_lock: direct_context mutex \
                 poisoned"
            );
            drop(image);
        }
    }
}

/// Compile-time invariant: Skia views must NOT impl
/// [`streamlib_adapter_abi::CpuReadable`] or
/// [`streamlib_adapter_abi::CpuWritable`].
///
/// Switching to `streamlib-adapter-cpu-readback` is the contractual
/// signal for "I want CPU bytes" — see #514. This macro stamps out
/// the type-level "ambiguous-impl" trick that turns an accidental
/// `impl CpuReadable for ...` into a compile error.
///
/// Pass the view types as a comma-separated list; the macro generates
/// a private `mod _assert_*` for each one. Trailing comma is fine.
macro_rules! assert_skia_views_not_cpu_readable {
    ($module:ident, $($ty:ty),+ $(,)?) => {
        mod $module {
            #[allow(unused_imports)]
            use super::*;
            use streamlib_adapter_abi::CpuReadable;

            trait AmbiguousIfImpl<A> {
                fn some_item() {}
            }
            impl<T: ?Sized> AmbiguousIfImpl<()> for T {}
            #[allow(dead_code)]
            struct Invalid;
            impl<T: ?Sized + CpuReadable> AmbiguousIfImpl<Invalid> for T {}

            const _: fn() = || {
                $(
                    let _ = <$ty as AmbiguousIfImpl<_>>::some_item;
                )+
            };
        }
    };
}

pub(crate) use assert_skia_views_not_cpu_readable;
