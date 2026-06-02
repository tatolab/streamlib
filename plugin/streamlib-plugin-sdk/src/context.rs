// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-arm twins of the engine's capability-typed context views.
//!
//! These are `#[repr(C)]` layout-matched copies of the engine's
//! [`RuntimeContextFullAccess`] / [`RuntimeContextLimitedAccess`] /
//! [`GpuContextFullAccess`] / [`GpuContextLimitedAccess`]. The host
//! constructs a view, passes `&view as *const _ as *const c_void`
//! across the plugin ABI, and the cdylib casts it straight back —
//! reading the host-built struct's fields directly. That is sound only
//! because both sides compile the SAME `#[repr(C)]` layout. The layout
//! tests below pin the byte shape against the engine's identical
//! assertions; a field added to one side but not the other trips a test
//! rather than corrupting field reads at runtime.
//!
//! Only the GPU-accessor field reads are provided. The ABI-mediated
//! accessors (`runtime_id`, `processor_id`, `audio_clock`, `runtime`,
//! …) are a later phase — the proof CPU-only plugin needs only the two
//! `gpu_*_access()` field reads on the RuntimeContext views.

use std::ffi::c_void;
use std::marker::PhantomData;

use streamlib_plugin_abi::{
    GpuContextFullAccessVTable, GpuContextLimitedAccessVTable, RuntimeContextVTable,
};

// =============================================================================
// GpuContextLimitedAccess — cdylib arm
// =============================================================================

/// Restricted GPU capability shim with ABI-stable `(handle, vtable)`
/// shape. Cdylib-arm twin of the engine's `GpuContextLimitedAccess`.
#[repr(C)]
pub struct GpuContextLimitedAccess {
    pub(crate) handle: *const c_void,
    pub(crate) vtable: *const GpuContextLimitedAccessVTable,
}

// SAFETY: `handle` points at a host-owned `Box<Arc<GpuContext>>` that is
// `Send + Sync`; the vtable pointer is `&'static` for the host's lifetime.
// Every method reaches the GpuContext through the handle via the vtable.
unsafe impl Send for GpuContextLimitedAccess {}
unsafe impl Sync for GpuContextLimitedAccess {}

impl Clone for GpuContextLimitedAccess {
    /// plugin-ABI-safe Clone. Dispatches through
    /// [`GpuContextLimitedAccessVTable::clone_handle`] to bump the
    /// host's `Arc<GpuContext>` refcount.
    fn clone(&self) -> Self {
        let new_handle = if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: handle + vtable were paired at construction and the
            // host's `clone_handle` callback contractually returns a fresh
            // owning pointer the matching `drop_handle` releases.
            unsafe { ((*self.vtable).clone_handle)(self.handle) }
        } else {
            std::ptr::null()
        };
        Self {
            handle: new_handle,
            vtable: self.vtable,
        }
    }
}

impl Drop for GpuContextLimitedAccess {
    /// Releases the host-owned handle via
    /// [`GpuContextLimitedAccessVTable::drop_handle`].
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: handle was produced by the host's `new()` /
            // `clone_handle`; the matching `drop_handle` callback runs
            // `Box::from_raw + drop` on the host side.
            unsafe { ((*self.vtable).drop_handle)(self.handle) };
        }
    }
}

// =============================================================================
// HandleKind — drop discriminator on GpuContextFullAccess
// =============================================================================

/// Discriminator for [`GpuContextFullAccess`]'s `handle` field. The
/// engine-internal in-process constructor sets `Boxed`; the cdylib
/// vtable-dispatched constructor sets `ScopeToken`. Drop dispatches on
/// this kind.
// The SDK never *constructs* a HandleKind (host-built views arrive by
// pointer); the variants exist for `#[repr(C)]` layout parity and the Drop
// match. Allow them to be "never constructed" within this crate.
#[allow(dead_code)]
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HandleKind {
    /// Handle is a host-allocated `Box<Arc<GpuContext>>`. The SDK never
    /// constructs this variant — only the host does.
    Boxed = 0,
    /// Handle is an opaque scope token from the host's
    /// `GpuContextLimitedAccessVTable::escalate_begin` callback.
    ScopeToken = 1,
}

// =============================================================================
// GpuContextFullAccess — cdylib arm
// =============================================================================

/// Privileged GPU capability shim with ABI-stable shape. Cdylib-arm twin
/// of the engine's `GpuContextFullAccess`.
///
/// Deliberately **not** `Clone` — a `&GpuContextFullAccess` is borrowed
/// from a [`RuntimeContextFullAccess`] for the duration of a single
/// lifecycle call and cannot be stashed.
///
/// ```compile_fail
/// fn assert_not_clone<T: Clone>() {}
/// assert_not_clone::<streamlib_plugin_sdk::sdk::context::GpuContextFullAccess>();
/// ```
#[repr(C)]
pub struct GpuContextFullAccess {
    pub(crate) handle: *const c_void,
    pub(crate) vtable: *const GpuContextFullAccessVTable,
    /// Drop discriminator. The cdylib only ever receives
    /// [`HandleKind::ScopeToken`] instances (built by the host's escalate
    /// path); the [`HandleKind::Boxed`] arm exists only for layout parity.
    pub(crate) handle_kind: HandleKind,
    /// Inherited LimitedAccess handle (scope-token mode only). `null` in
    /// Boxed mode.
    pub(crate) inherited_lim_handle: *const c_void,
    /// Inherited LimitedAccess vtable pointer paired with
    /// [`Self::inherited_lim_handle`]. `null` in Boxed mode.
    pub(crate) inherited_lim_vtable: *const GpuContextLimitedAccessVTable,
}

// SAFETY: same shape as the engine twin. The handle is a host-owned
// `Box<Arc<GpuContext>>` or an opaque scope token (both `Send + Sync`);
// the vtable pointers are `&'static`; the inherited LimitedAccess fields
// either borrow the originating LimitedAccess's host handle or are null.
unsafe impl Send for GpuContextFullAccess {}
unsafe impl Sync for GpuContextFullAccess {}

impl Drop for GpuContextFullAccess {
    /// Releases the handle.
    ///
    /// The cdylib only ever holds [`HandleKind::ScopeToken`] instances,
    /// whose cleanup is the authority of the host's `escalate_end`
    /// callback — so Drop is a no-op here. The [`HandleKind::Boxed`] arm
    /// is unreachable in the SDK (the SDK never constructs a Boxed
    /// handle), so it is also a no-op rather than naming the engine's
    /// `Arc<GpuContext>`.
    fn drop(&mut self) {
        if self.handle.is_null() {
            return;
        }
        match self.handle_kind {
            // The SDK never constructs a Boxed handle (that requires the
            // engine's `Arc<GpuContext>`). Unreachable in cdylib code.
            HandleKind::Boxed => {}
            // No-op — escalate_end is the authority.
            HandleKind::ScopeToken => {}
        }
    }
}

// =============================================================================
// RuntimeContextFullAccess — cdylib arm
// =============================================================================

/// Privileged-`RuntimeContext` view passed to `setup` / `teardown` /
/// Manual-mode `start` / `stop`. Cdylib-arm twin of the engine's
/// `RuntimeContextFullAccess`.
///
/// Deliberately `!Clone` and borrow-scoped.
///
/// ```compile_fail
/// fn assert_not_clone<T: Clone>() {}
/// assert_not_clone::<streamlib_plugin_sdk::sdk::context::RuntimeContextFullAccess<'static>>();
/// ```
#[repr(C)]
pub struct RuntimeContextFullAccess<'a> {
    /// Opaque pointer to the host-owned `RuntimeContext`.
    handle: *const c_void,
    /// Pointer to the host's [`RuntimeContextVTable`].
    vtable: *const RuntimeContextVTable,
    gpu_full: GpuContextFullAccess,
    gpu_limited: GpuContextLimitedAccess,
    _marker: PhantomData<&'a ()>,
}

// SAFETY: same shape as the engine twin; every field is an opaque
// pointer / `Send + Sync` embedded view. The host builds the value and
// keeps the backing alive for the borrow's lifetime.
unsafe impl Send for RuntimeContextFullAccess<'_> {}
unsafe impl Sync for RuntimeContextFullAccess<'_> {}

impl<'a> RuntimeContextFullAccess<'a> {
    /// Privileged GPU capability — allocations, device-wide ops, escalate.
    pub fn gpu_full_access(&self) -> &GpuContextFullAccess {
        &self.gpu_full
    }

    /// Restricted GPU capability. Cloneable — hand to a Manual-mode worker
    /// thread during `start()` so it can participate in the hot path with
    /// limited-access operations only.
    pub fn gpu_limited_access(&self) -> &GpuContextLimitedAccess {
        &self.gpu_limited
    }
}

// =============================================================================
// RuntimeContextLimitedAccess — cdylib arm
// =============================================================================

/// Restricted-`RuntimeContext` view passed to `process` / `on_pause` /
/// `on_resume`. Cdylib-arm twin of the engine's
/// `RuntimeContextLimitedAccess`.
///
/// Deliberately `!Clone` and borrow-scoped. `gpu_full_access()` is
/// intentionally absent — a `process()` body cannot reach privileged GPU
/// operations.
///
/// ```compile_fail
/// fn assert_not_clone<T: Clone>() {}
/// assert_not_clone::<streamlib_plugin_sdk::sdk::context::RuntimeContextLimitedAccess<'static>>();
/// ```
///
/// ```compile_fail
/// fn reach_full(ctx: &streamlib_plugin_sdk::sdk::context::RuntimeContextLimitedAccess<'_>) {
///     let _ = ctx.gpu_full_access();
/// }
/// ```
#[repr(C)]
pub struct RuntimeContextLimitedAccess<'a> {
    handle: *const c_void,
    vtable: *const RuntimeContextVTable,
    gpu_limited: GpuContextLimitedAccess,
    _marker: PhantomData<&'a ()>,
}

// SAFETY: see [`RuntimeContextFullAccess`].
unsafe impl Send for RuntimeContextLimitedAccess<'_> {}
unsafe impl Sync for RuntimeContextLimitedAccess<'_> {}

impl<'a> RuntimeContextLimitedAccess<'a> {
    /// Restricted GPU capability — cheap, pool-backed, non-allocating ops.
    pub fn gpu_limited_access(&self) -> &GpuContextLimitedAccess {
        &self.gpu_limited
    }
}

// =============================================================================
// Cross-crate layout lock
// =============================================================================
//
// These views cross the plugin ABI by raw-pointer cast between the host
// build and a separately-built plugin. They are `#[repr(C)]` so the
// layout is identical across builds; these assertions pin the byte shape
// to the SAME numbers the engine asserts in
// `core/context/runtime_context.rs`. A field added to one side but not the
// other trips a test rather than corrupting field reads at runtime.
#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn gpu_context_view_sizes_are_pinned() {
        assert_eq!(size_of::<GpuContextFullAccess>(), 40);
        assert_eq!(align_of::<GpuContextFullAccess>(), 8);
        assert_eq!(size_of::<GpuContextLimitedAccess>(), 16);
        assert_eq!(align_of::<GpuContextLimitedAccess>(), 8);
    }

    #[test]
    fn runtime_context_full_access_layout() {
        assert_eq!(size_of::<RuntimeContextFullAccess<'static>>(), 72);
        assert_eq!(align_of::<RuntimeContextFullAccess<'static>>(), 8);
        assert_eq!(offset_of!(RuntimeContextFullAccess<'static>, handle), 0);
        assert_eq!(offset_of!(RuntimeContextFullAccess<'static>, vtable), 8);
        assert_eq!(offset_of!(RuntimeContextFullAccess<'static>, gpu_full), 16);
        assert_eq!(offset_of!(RuntimeContextFullAccess<'static>, gpu_limited), 56);
    }

    #[test]
    fn runtime_context_limited_access_layout() {
        assert_eq!(size_of::<RuntimeContextLimitedAccess<'static>>(), 32);
        assert_eq!(align_of::<RuntimeContextLimitedAccess<'static>>(), 8);
        assert_eq!(offset_of!(RuntimeContextLimitedAccess<'static>, handle), 0);
        assert_eq!(offset_of!(RuntimeContextLimitedAccess<'static>, vtable), 8);
        assert_eq!(offset_of!(RuntimeContextLimitedAccess<'static>, gpu_limited), 16);
    }
}
