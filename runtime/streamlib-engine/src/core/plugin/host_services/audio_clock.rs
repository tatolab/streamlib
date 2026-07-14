// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `AudioClockVTable` callbacks + static vtable + accessor.
//!
//! Paired with the per-RuntimeContext audio-clock handle returned by
//! [`HOST_RUNTIME_CONTEXT_VTABLE`]`::audio_clock_handle`. The cdylib
//! treats the handle as opaque and dispatches through fn pointers;
//! `OnTickBridge` owns the cdylib's `(callback, user_data,
//! drop_user_data)` trio for the lifetime of the on-tick
//! registration.
//!
//! [`HOST_RUNTIME_CONTEXT_VTABLE`]: super::HOST_RUNTIME_CONTEXT_VTABLE

use std::ffi::c_void;

use streamlib_plugin_abi::{AUDIO_CLOCK_VTABLE_LAYOUT_VERSION, AudioClockVTable};

use crate::core::context::SharedAudioClock;

use super::host_callbacks;
use super::run_host_extern_c;

unsafe extern "C" fn host_acv_sample_rate(handle: *const c_void) -> u32 {
    run_host_extern_c(
        "host_acv_sample_rate",
        || {
            if handle.is_null() {
                return 0;
            }
            let clock = unsafe { &*(handle as *const SharedAudioClock) };
            clock.sample_rate()
        },
        0,
    )
}

unsafe extern "C" fn host_acv_buffer_size(handle: *const c_void) -> usize {
    run_host_extern_c(
        "host_acv_buffer_size",
        || {
            if handle.is_null() {
                return 0;
            }
            let clock = unsafe { &*(handle as *const SharedAudioClock) };
            clock.buffer_size()
        },
        0,
    )
}

unsafe extern "C" fn host_acv_on_tick(
    handle: *const c_void,
    callback: unsafe extern "C" fn(*mut c_void, streamlib_plugin_abi::AudioTickContextRepr),
    user_data: *mut c_void,
    drop_user_data: unsafe extern "C" fn(*mut c_void),
) {
    run_host_extern_c(
        "host_acv_on_tick",
        || {
            // [`OnTickBridge`] owns the (callback, user_data,
            // drop_user_data) trio. Its `Drop` impl fires
            // `drop_user_data` exactly once, no matter where the
            // bridge ends up: stored on the clock (success path —
            // drop fires at clock teardown), dropped immediately
            // (null-handle path or panic before move), or dropped on
            // the unwind path between move and `clock.on_tick`
            // returning (panic-recovery path — the bridge moved into
            // `cb` drops when `cb` unwinds).
            //
            // This shape is the sole owner of the cleanup: the
            // wrapper's third argument to `run_host_extern_c` MUST
            // stay `()` (no `drop_user_data` call) — Rust evaluates
            // function arguments eagerly, so a third-arg side effect
            // would fire `drop_user_data` unconditionally before the
            // body even runs, double-firing it on every success path.
            let bridge = OnTickBridge {
                callback,
                user_data,
                drop_user_data,
            };
            if handle.is_null() {
                // Bridge drops here → drop_user_data fires once. The
                // explicit `drop(bridge)` is for clarity; lexical
                // scope alone would fire it on the same line.
                drop(bridge);
                return;
            }
            let clock = unsafe { &*(handle as *const SharedAudioClock) };

            // Bridge moves into the boxed closure. If
            // `clock.on_tick(cb)` panics before storing `cb`, the
            // unwind drops the Box → closure → bridge →
            // drop_user_data fires exactly once. If `clock.on_tick`
            // stores `cb` successfully, the bridge lives until the
            // clock tears down; drop_user_data fires then.
            let cb: Box<dyn Fn(crate::core::context::AudioTickContext) + Send + Sync> =
                Box::new(move |ctx_local| bridge.fire(ctx_local));
            clock.on_tick(cb);
        },
        // Intentional `()`: the cleanup contract is held entirely by
        // `OnTickBridge::Drop`. See body comment.
        (),
    )
}

/// Holder for the cdylib's `(callback, user_data, drop_user_data)`
/// trio. Owns the user-data pointer for the lifetime of the on-tick
/// registration; the deleter fires when the registration drops.
struct OnTickBridge {
    callback: unsafe extern "C" fn(*mut c_void, streamlib_plugin_abi::AudioTickContextRepr),
    user_data: *mut c_void,
    drop_user_data: unsafe extern "C" fn(*mut c_void),
}

// SAFETY: cdylib's ABI contract requires the callback + drop pair to be
// thread-safe. The on-tick callback may fire from any thread the host's
// audio clock chooses (today, the audio-clock thread).
unsafe impl Send for OnTickBridge {}
unsafe impl Sync for OnTickBridge {}

impl OnTickBridge {
    fn fire(&self, ctx: crate::core::context::AudioTickContext) {
        let repr = streamlib_plugin_abi::AudioTickContextRepr {
            timestamp_ns: ctx.timestamp_ns,
            samples_needed: ctx.samples_needed as u64,
            sample_rate: ctx.sample_rate,
            _reserved_padding: 0,
            tick_number: ctx.tick_number,
        };
        // SAFETY: callback + user_data come from the cdylib's ABI
        // promise; valid for the lifetime of this bridge.
        unsafe { (self.callback)(self.user_data, repr) };
    }
}

impl Drop for OnTickBridge {
    fn drop(&mut self) {
        // SAFETY: drop_user_data is part of the cdylib's ABI contract
        // and is called exactly once when this bridge is released.
        unsafe { (self.drop_user_data)(self.user_data) };
    }
}

/// Static [`AudioClockVTable`] installed once per process. Paired
/// with the per-RuntimeContext audio-clock handle returned by
/// `HOST_RUNTIME_CONTEXT_VTABLE::audio_clock_handle`.
pub static HOST_AUDIO_CLOCK_VTABLE: AudioClockVTable = AudioClockVTable {
    layout_version: AUDIO_CLOCK_VTABLE_LAYOUT_VERSION,
    _reserved_padding: 0,
    sample_rate: host_acv_sample_rate,
    buffer_size: host_acv_buffer_size,
    on_tick: host_acv_on_tick,
};

/// Pointer to the [`AudioClockVTable`] this DSO should dispatch
/// through. Same DSO-routing rule as
/// [`super::host_runtime_context_vtable`]: cdylib reads the host's
/// pointer from the cache populated by `install_host_services`; host
/// falls back to its local static.
pub fn host_audio_clock_vtable() -> *const AudioClockVTable {
    match host_callbacks() {
        Some(c) if !c.audio_clock_vtable.is_null() => c.audio_clock_vtable,
        _ => &HOST_AUDIO_CLOCK_VTABLE,
    }
}

#[cfg(test)]
mod audio_clock_vtable_null_handle_guards {
    //! Regression locks for the null-handle guards added to the
    //! `AudioClockVTable` callbacks. Same shape as the
    //! `RuntimeContextVTable` guards module: mental-revert removes
    //! the guard, the wrapper SIGSEGVs the test runner.
    //!
    //! `on_tick`'s guard additionally invokes `drop_user_data` so the
    //! cdylib's boxed `user_data` doesn't leak — verified via a
    //! `Drop`-counting fixture.

    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn sample_rate_returns_zero_on_null_handle() {
        let v = unsafe { (HOST_AUDIO_CLOCK_VTABLE.sample_rate)(std::ptr::null()) };
        assert_eq!(v, 0);
    }

    #[test]
    fn buffer_size_returns_zero_on_null_handle() {
        let v = unsafe { (HOST_AUDIO_CLOCK_VTABLE.buffer_size)(std::ptr::null()) };
        assert_eq!(v, 0);
    }

    /// Counter shared with the on_tick test's `drop_user_data` callback
    /// so the test can assert the user_data reclamation actually fires.
    static ON_TICK_DROP_COUNT: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn dummy_tick_callback(
        _user_data: *mut c_void,
        _ctx: streamlib_plugin_abi::AudioTickContextRepr,
    ) {
        // Never fires in the null-handle test — the host short-circuits
        // before registering.
    }

    unsafe extern "C" fn counting_drop_user_data(user_data: *mut c_void) {
        ON_TICK_DROP_COUNT.fetch_add(1, Ordering::SeqCst);
        // SAFETY: in this test we leaked a `Box<u8>` into user_data;
        // reclaim it here.
        if !user_data.is_null() {
            unsafe {
                let _ = Box::from_raw(user_data as *mut u8);
            }
        }
    }

    #[test]
    fn on_tick_drops_user_data_on_null_handle() {
        // Mental-revert: without the null-handle guard the wrapper
        // would still construct the bridge (now hoisted above the
        // null check), then deref a null `*const SharedAudioClock`
        // to call `clock.on_tick(...)` and SIGSEGV before the bridge
        // could move into `cb`. With the guard, the bridge drops
        // before the deref → `drop_user_data` fires exactly once.
        //
        // Mental-revert for the bigger fix in the same commit
        // (removing `drop_user_data` from the wrapper's third arg
        // and hoisting bridge construction above the null check):
        // restoring the old third-arg block-expression cleanup would
        // re-introduce the eager-arg-eval double-fire on every
        // call (success or null) — this test would observe
        // `after == before + 2` and fail.
        let before = ON_TICK_DROP_COUNT.load(Ordering::SeqCst);
        // Leak a Box<u8> so counting_drop_user_data has something to
        // reclaim (mirrors cdylib's Box<oneshot::Sender>-shaped pattern).
        let user_data = Box::into_raw(Box::new(0u8)) as *mut c_void;
        unsafe {
            (HOST_AUDIO_CLOCK_VTABLE.on_tick)(
                std::ptr::null(),
                dummy_tick_callback,
                user_data,
                counting_drop_user_data,
            );
        }
        let after = ON_TICK_DROP_COUNT.load(Ordering::SeqCst);
        assert_eq!(
            after,
            before + 1,
            "drop_user_data must fire exactly once on null-handle short-circuit"
        );
    }

    /// Success-path companion to the null-handle test. Exercises the
    /// real `clock.on_tick(...)` storage path with a tiny ad-hoc
    /// `SharedAudioClock` and asserts `drop_user_data` fires exactly
    /// once across the full lifecycle (registration → clock drop).
    /// Locks the eager-arg-eval double-free fix: restoring the old
    /// third-arg block-expression cleanup would observe `after ==
    /// before + 2` and fail this test.
    #[test]
    fn on_tick_drops_user_data_exactly_once_on_success_path() {
        use crate::core::context::{AudioClockConfig, SharedAudioClock, SoftwareAudioClock};
        use std::sync::Arc as StdArc;
        let before = ON_TICK_DROP_COUNT.load(Ordering::SeqCst);
        let user_data = Box::into_raw(Box::new(0u8)) as *mut c_void;
        // Build a tiny clock just for this test. Drops at the end
        // of the function, firing the bridge's Drop (which fires
        // drop_user_data exactly once if the fix holds).
        let clock: SharedAudioClock =
            StdArc::new(SoftwareAudioClock::new(AudioClockConfig::new(48_000, 512)));
        let handle = &clock as *const SharedAudioClock as *const c_void;
        unsafe {
            (HOST_AUDIO_CLOCK_VTABLE.on_tick)(
                handle,
                dummy_tick_callback,
                user_data,
                counting_drop_user_data,
            );
        }
        // Drop the clock before reading the counter so the bridge's
        // Drop fires deterministically.
        drop(clock);
        let after = ON_TICK_DROP_COUNT.load(Ordering::SeqCst);
        assert_eq!(
            after,
            before + 1,
            "drop_user_data must fire exactly once across the full \
             on_tick lifecycle (registration → clock drop); \
             `before + 2` indicates the eager-arg-eval double-free \
             regressed"
        );
    }
}

#[cfg(test)]
mod audio_clock_vtable_tier1_wire_format_tests {
    //! Tier-1 wire-format tests for [`HOST_AUDIO_CLOCK_VTABLE`].
    //!
    //! Per-callback null-handle coverage lives in
    //! [`audio_clock_vtable_null_handle_guards`] above (3 tests —
    //! `sample_rate`, `buffer_size`, `on_tick`). The on_tick
    //! single-fire invariant is locked twice over: once on the
    //! null-handle path, once on the success path against a real
    //! `SoftwareAudioClock`. This module adds the
    //! `layout_version_matches_constant` lock.
    //!
    //! `sample_rate` / `buffer_size` are primitive-returning and
    //! take no out-param; `on_tick` takes a callback trio whose
    //! ownership semantics are covered by the null-handle and
    //! success-path tests in the guards module. The "null
    //! out-param" / "invalid input" tier-1 categories don't apply.

    use super::*;

    #[test]
    fn layout_version_matches_constant() {
        assert_eq!(
            HOST_AUDIO_CLOCK_VTABLE.layout_version,
            streamlib_plugin_abi::AUDIO_CLOCK_VTABLE_LAYOUT_VERSION,
        );
    }
}
