// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-side wrapper around the host's `SharedAudioClock`.
//!
//! Constructed by the runtime-context shim's `audio_clock()` accessor
//! from a per-RuntimeContext `(handle, vtable)` pair pulled out of the
//! engine's [`RuntimeContextVTable`] and the
//! [`AudioClockVTable`](streamlib_plugin_abi::AudioClockVTable)
//! installed on `HostServices`. The host's static vtable callbacks
//! delegate to the underlying `Arc<dyn AudioClock>`.

use std::ffi::c_void;

use streamlib_plugin_abi::{AudioClockVTable, AudioTickContextRepr};

use super::AudioTickContext;

/// Cdylib-side handle to the host's audio clock. Carries an opaque
/// host-owned handle plus the vtable that drives it; the layout is
/// identical between host and cdylib because both pointers point at
/// host-owned state via stable extern "C" callbacks.
///
/// Lifetimes: the handle is valid for the lifetime of the
/// [`super::RuntimeContextFullAccess`] / `LimitedAccess` shim it was
/// obtained from. The shim's borrow checker enforces this — the
/// audio-clock shim cannot outlive the ctx borrow because it borrows
/// the same lifetime.
#[derive(Clone, Copy)]
pub struct AudioClockShim<'a> {
    handle: *const c_void,
    vtable: *const AudioClockVTable,
    _marker: std::marker::PhantomData<&'a ()>,
}

// Pointer fields are Send/Sync by ABI promise — host guarantees the
// handle outlives the call boundary and the vtable's callbacks are
// thread-safe. The borrow lifetime on the shim is what actually keeps
// it scoped.
unsafe impl<'a> Send for AudioClockShim<'a> {}
unsafe impl<'a> Sync for AudioClockShim<'a> {}

impl<'a> AudioClockShim<'a> {
    /// Construct a shim from a host-supplied handle + vtable. Crate-
    /// internal: the runtime-context shim is the only legitimate
    /// builder.
    pub(crate) fn from_ffi(handle: *const c_void, vtable: *const AudioClockVTable) -> Self {
        Self {
            handle,
            vtable,
            _marker: std::marker::PhantomData,
        }
    }

    /// Sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        // SAFETY: vtable + handle promised valid by the engine shim
        // for the lifetime of `'a`.
        unsafe { ((*self.vtable).sample_rate)(self.handle) }
    }

    /// Number of samples per tick.
    pub fn buffer_size(&self) -> usize {
        unsafe { ((*self.vtable).buffer_size)(self.handle) }
    }

    /// Register a callback invoked on every audio tick. The callback
    /// fires on the host's audio-clock thread; it must be `Send +
    /// Sync` and complete quickly enough to avoid missing the next
    /// tick window.
    ///
    /// The registration outlives this call — the host owns the
    /// callback until clock teardown. Multiple registrations are
    /// permitted and fire in registration order.
    pub fn on_tick<F>(&self, callback: F)
    where
        F: Fn(AudioTickContext) + Send + Sync + 'static,
    {
        // Box the user closure on the cdylib's heap. The host carries
        // a raw pointer through extern "C" and fires `tick_trampoline`
        // for each tick; `drop_trampoline` runs at clock teardown.
        let boxed: Box<dyn Fn(AudioTickContext) + Send + Sync + 'static> = Box::new(callback);
        // Double-box: outer Box owns a heap allocation whose stable
        // address survives moves; inner Box is the trait object.
        let user_data = Box::into_raw(Box::new(boxed)) as *mut c_void;
        // SAFETY: vtable promises the on_tick callback hands the
        // user_data + drop pair to its audio-clock impl; the host
        // never deref's user_data itself.
        unsafe {
            ((*self.vtable).on_tick)(
                self.handle,
                audio_clock_shim_tick_trampoline,
                user_data,
                audio_clock_shim_drop_trampoline,
            );
        }
    }
}

/// Trampoline the host invokes per tick. Casts the cdylib's boxed
/// closure back from the opaque `user_data` pointer and calls it with
/// a converted [`AudioTickContext`].
unsafe extern "C" fn audio_clock_shim_tick_trampoline(
    user_data: *mut c_void,
    repr: AudioTickContextRepr,
) {
    if user_data.is_null() {
        return;
    }
    // SAFETY: `user_data` was supplied by `AudioClockShim::on_tick`
    // and is a `*mut Box<dyn Fn(AudioTickContext) + Send + Sync>`. The
    // pointer is valid until `audio_clock_shim_drop_trampoline` runs.
    let closure = unsafe {
        &*(user_data as *const Box<dyn Fn(AudioTickContext) + Send + Sync>)
    };
    let ctx = AudioTickContext {
        timestamp_ns: repr.timestamp_ns,
        samples_needed: repr.samples_needed as usize,
        sample_rate: repr.sample_rate,
        tick_number: repr.tick_number,
    };
    closure(ctx);
}

/// Drop trampoline the host invokes when the on-tick registration is
/// released. Reclaims the cdylib's boxed closure.
unsafe extern "C" fn audio_clock_shim_drop_trampoline(user_data: *mut c_void) {
    if user_data.is_null() {
        return;
    }
    // SAFETY: pair with `Box::into_raw` in `AudioClockShim::on_tick`.
    unsafe {
        let _ = Box::from_raw(
            user_data as *mut Box<dyn Fn(AudioTickContext) + Send + Sync>,
        );
    }
}
