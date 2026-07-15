// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-arm twin of the engine's audio-clock shim + tick-context view.
//!
//! Engine-free mirror of the engine's
//! `core::context::audio_clock_shim::AudioClockShim` (and the
//! `core::context::audio_clock::AudioTickContext` value struct). The host
//! owns the `Arc<dyn AudioClock>`; the cdylib obtains an opaque
//! `(handle, vtable)` pair from a runtime-context view's `audio_clock()`
//! accessor and dispatches through the host's static
//! [`AudioClockVTable`](streamlib_plugin_abi::AudioClockVTable) installed
//! on `HostServices`.
//!
//! The only value crossing the plugin ABI is
//! [`AudioTickContextRepr`](streamlib_plugin_abi::AudioTickContextRepr)
//! (already `#[repr(C)]` + layout-locked in the ABI crate); the tick
//! trampoline converts it into the engine-free [`AudioTickContext`] a
//! plugin author sees. `AudioClockShim` itself does NOT cross the ABI as
//! a struct — it is reconstructed cdylib-side from the pair, so it needs
//! no layout lock.

use std::ffi::c_void;

use streamlib_adapter_abi::ffi::run_host_extern_c;
use streamlib_plugin_abi::{AudioClockVTable, AudioTickContextRepr};

/// Timing context handed to an [`AudioClockShim::on_tick`] callback on
/// every audio tick. Engine-free value twin of the engine's
/// `core::context::AudioTickContext`; constructed cdylib-side in the tick
/// trampoline from the ABI's
/// [`AudioTickContextRepr`](streamlib_plugin_abi::AudioTickContextRepr).
#[derive(Debug, Clone, Copy)]
pub struct AudioTickContext {
    /// Monotonic timestamp in nanoseconds.
    pub timestamp_ns: i64,
    /// Number of samples to produce this tick (per channel).
    pub samples_needed: usize,
    /// Sample rate of the clock in Hz.
    pub sample_rate: u32,
    /// Tick number (starts at 0, increments each tick).
    pub tick_number: u64,
}

/// Cdylib-side handle to the host's audio clock. Carries an opaque
/// host-owned handle plus the vtable that drives it; the layout is
/// identical between host and cdylib because both pointers point at
/// host-owned state via stable extern "C" callbacks.
///
/// Lifetimes: the handle is valid for the lifetime of the
/// runtime-context shim (`RuntimeContextFullAccess` /
/// `RuntimeContextLimitedAccess`) it was obtained from. The shim's
/// borrow checker enforces this — the audio-clock shim borrows the same
/// lifetime and cannot outlive the ctx borrow.
#[derive(Clone, Copy)]
pub struct AudioClockShim<'a> {
    handle: *const c_void,
    vtable: *const AudioClockVTable,
    _marker: std::marker::PhantomData<&'a ()>,
}

// SAFETY: pointer fields are Send/Sync by ABI promise — the host
// guarantees the handle outlives the call boundary and the vtable's
// callbacks are thread-safe. The borrow lifetime on the shim is what
// actually keeps it scoped.
unsafe impl<'a> Send for AudioClockShim<'a> {}
unsafe impl<'a> Sync for AudioClockShim<'a> {}

impl<'a> AudioClockShim<'a> {
    /// Construct a shim from a host-supplied handle + vtable. Crate-
    /// internal: the runtime-context views are the only legitimate
    /// builders.
    pub(crate) fn from_ffi(handle: *const c_void, vtable: *const AudioClockVTable) -> Self {
        Self {
            handle,
            vtable,
            _marker: std::marker::PhantomData,
        }
    }

    /// Sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        // `audio_clock_vtable` is ABI-optional: the install handshake
        // validates it only-when-non-null, so a host that installs no audio
        // clock leaves the vtable null and `audio_clock()` hands us a null
        // pointer. Guard the deref rather than SIGSEGV across the plugin ABI
        // (mirrors `GpuContextFullAccess::color_converter`'s null-vtable
        // discipline); with no host clock the cadence is simply zero.
        if self.vtable.is_null() {
            return 0;
        }
        // SAFETY: vtable + handle promised valid by the runtime-context
        // view for the lifetime of `'a`.
        unsafe { ((*self.vtable).sample_rate)(self.handle) }
    }

    /// Number of samples per tick.
    pub fn buffer_size(&self) -> usize {
        // See [`Self::sample_rate`] — null `audio_clock_vtable` is reachable
        // and reads as a zero cadence rather than dereferencing null.
        if self.vtable.is_null() {
            return 0;
        }
        // SAFETY: see [`Self::sample_rate`].
        unsafe { ((*self.vtable).buffer_size)(self.handle) }
    }

    /// Register a callback invoked on every audio tick. The callback
    /// fires on the host's audio-clock thread; it must be `Send + Sync`
    /// and complete quickly enough to avoid missing the next tick window.
    ///
    /// The registration outlives this call — the host owns the callback
    /// until clock teardown. Multiple registrations are permitted and
    /// fire in registration order.
    pub fn on_tick<F>(&self, callback: F)
    where
        F: Fn(AudioTickContext) + Send + Sync + 'static,
    {
        // Box the user closure on the cdylib's heap. The host carries a
        // raw pointer through extern "C" and fires
        // `audio_clock_shim_tick_trampoline` for each tick;
        // `audio_clock_shim_drop_trampoline` runs at clock teardown.
        let boxed: Box<dyn Fn(AudioTickContext) + Send + Sync + 'static> = Box::new(callback);
        // Double-box: outer Box owns a heap allocation whose stable
        // address survives moves; inner Box is the trait object.
        let user_data = Box::into_raw(Box::new(boxed)) as *mut c_void;
        // Null `audio_clock_vtable` is reachable (see [`Self::sample_rate`]):
        // there is no host clock to register with, so `on_tick` is a no-op —
        // but it must still reclaim the boxed closure here so the user's
        // captured state drops exactly once instead of leaking.
        if self.vtable.is_null() {
            // SAFETY: pair with the `Box::into_raw` above; nothing else holds
            // `user_data`.
            unsafe {
                let _ =
                    Box::from_raw(user_data as *mut Box<dyn Fn(AudioTickContext) + Send + Sync>);
            }
            return;
        }
        // SAFETY: the vtable's on_tick callback hands the user_data + drop
        // pair to the host's audio-clock impl; the host never deref's
        // user_data itself.
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

/// Trampoline the host invokes per tick. Casts the cdylib's boxed closure
/// back from the opaque `user_data` pointer and calls it with a converted
/// [`AudioTickContext`]. Wrapped in the panic-safety net so a panic in the
/// plugin's closure never unwinds across the plugin ABI into host code.
unsafe extern "C" fn audio_clock_shim_tick_trampoline(
    user_data: *mut c_void,
    repr: AudioTickContextRepr,
) {
    run_host_extern_c(
        "audio_clock_shim_tick_trampoline",
        || {
            if user_data.is_null() {
                return;
            }
            // SAFETY: `user_data` was supplied by `AudioClockShim::on_tick`
            // and is a `*mut Box<dyn Fn(AudioTickContext) + Send + Sync>`.
            // The pointer is valid until `audio_clock_shim_drop_trampoline`
            // runs.
            let closure =
                unsafe { &*(user_data as *const Box<dyn Fn(AudioTickContext) + Send + Sync>) };
            let ctx = AudioTickContext {
                timestamp_ns: repr.timestamp_ns,
                samples_needed: repr.samples_needed as usize,
                sample_rate: repr.sample_rate,
                tick_number: repr.tick_number,
            };
            closure(ctx);
        },
        (),
    )
}

/// Drop trampoline the host invokes when the on-tick registration is
/// released. Reclaims the cdylib's boxed closure exactly once. Wrapped in
/// the panic-safety net so a panic in a captured value's `Drop` never
/// unwinds across the plugin ABI.
unsafe extern "C" fn audio_clock_shim_drop_trampoline(user_data: *mut c_void) {
    run_host_extern_c(
        "audio_clock_shim_drop_trampoline",
        || {
            if user_data.is_null() {
                return;
            }
            // SAFETY: pair with `Box::into_raw` in `AudioClockShim::on_tick`.
            unsafe {
                let _ =
                    Box::from_raw(user_data as *mut Box<dyn Fn(AudioTickContext) + Send + Sync>);
            }
        },
        (),
    )
}

#[cfg(test)]
mod on_tick_round_trip_tests {
    //! Round-trips the cdylib shim's `on_tick` against a fake host
    //! `AudioClockVTable` that stores the registered `(callback,
    //! user_data, drop_user_data)` trio (mirroring the host's real
    //! `OnTickBridge` ownership), fires synthesized
    //! [`AudioTickContextRepr`] ticks through the tick trampoline, then
    //! releases the registration through the drop trampoline.
    //!
    //! Locks three invariants:
    //! - `AudioTickContextRepr` → [`AudioTickContext`] field round-trip
    //!   (incl. the `samples_needed as usize` narrowing) through the tick
    //!   trampoline.
    //! - `sample_rate()` / `buffer_size()` dispatch through the vtable.
    //! - the boxed closure is reclaimed EXACTLY ONCE, at registration
    //!   release — no leak (drop must fire) and no double-free (drop must
    //!   fire only once). Mental-revert: dropping the `Box::from_raw` in
    //!   the drop trampoline leaves the counter at 0; firing it twice
    //!   leaves it at 2. Either trips the assertion.

    use super::*;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// The `(callback, user_data, drop_user_data)` trio a real host stashes
    /// on its clock for the lifetime of the registration.
    struct RegisteredTrio {
        callback: unsafe extern "C" fn(*mut c_void, AudioTickContextRepr),
        user_data: *mut c_void,
        drop_user_data: unsafe extern "C" fn(*mut c_void),
    }

    thread_local! {
        static REGISTERED: RefCell<Option<RegisteredTrio>> = const { RefCell::new(None) };
    }

    unsafe extern "C" fn fake_sample_rate(_handle: *const c_void) -> u32 {
        48_000
    }

    unsafe extern "C" fn fake_buffer_size(_handle: *const c_void) -> usize {
        512
    }

    unsafe extern "C" fn fake_on_tick(
        _handle: *const c_void,
        callback: unsafe extern "C" fn(*mut c_void, AudioTickContextRepr),
        user_data: *mut c_void,
        drop_user_data: unsafe extern "C" fn(*mut c_void),
    ) {
        REGISTERED.with(|slot| {
            *slot.borrow_mut() = Some(RegisteredTrio {
                callback,
                user_data,
                drop_user_data,
            });
        });
    }

    static FAKE_VTABLE: AudioClockVTable = AudioClockVTable {
        layout_version: streamlib_plugin_abi::AUDIO_CLOCK_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        sample_rate: fake_sample_rate,
        buffer_size: fake_buffer_size,
        on_tick: fake_on_tick,
    };

    /// Fires its counter's increment when dropped — captured by the
    /// on-tick closure so the test can prove the closure (and thus the
    /// boxed user_data) is reclaimed exactly once.
    struct ClosureDropSpy(Arc<AtomicUsize>);
    impl Drop for ClosureDropSpy {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn on_tick_round_trips_repr_and_drops_user_data_exactly_once() {
        // Non-null dummy handle; the fake vtable ignores it.
        let handle = std::ptr::dangling::<c_void>();
        let shim = AudioClockShim::from_ffi(handle, &FAKE_VTABLE);

        // POD getters dispatch through the vtable.
        assert_eq!(shim.sample_rate(), 48_000);
        assert_eq!(shim.buffer_size(), 512);

        let received: Arc<Mutex<Vec<AudioTickContext>>> = Arc::new(Mutex::new(Vec::new()));
        let drop_counter = Arc::new(AtomicUsize::new(0));

        {
            let received_in_closure = Arc::clone(&received);
            let spy = ClosureDropSpy(Arc::clone(&drop_counter));
            shim.on_tick(move |tick| {
                // `spy` is owned by the closure (moved in) — it drops when
                // the closure is reclaimed by the drop trampoline.
                let _keep_spy_alive = &spy;
                received_in_closure.lock().unwrap().push(tick);
            });
        }

        assert!(
            REGISTERED.with(|slot| slot.borrow().is_some()),
            "host on_tick slot must have registered the trio",
        );

        // Fire two ticks host-style, through the tick trampoline.
        let reprs = [
            AudioTickContextRepr {
                timestamp_ns: 111,
                samples_needed: 512,
                sample_rate: 48_000,
                _reserved_padding: 0,
                tick_number: 0,
            },
            AudioTickContextRepr {
                timestamp_ns: 222,
                samples_needed: 256,
                sample_rate: 44_100,
                _reserved_padding: 0,
                tick_number: 1,
            },
        ];
        REGISTERED.with(|slot| {
            let borrow = slot.borrow();
            let trio = borrow.as_ref().unwrap();
            for repr in reprs {
                // SAFETY: the trio's callback is the shim's tick trampoline;
                // user_data is the boxed closure it registered.
                unsafe { (trio.callback)(trio.user_data, repr) };
            }
        });

        // Round-trip: the plugin's closure saw both ticks, fields intact.
        {
            let got = received.lock().unwrap();
            assert_eq!(got.len(), 2);
            assert_eq!(got[0].timestamp_ns, 111);
            assert_eq!(got[0].samples_needed, 512);
            assert_eq!(got[0].sample_rate, 48_000);
            assert_eq!(got[0].tick_number, 0);
            assert_eq!(got[1].timestamp_ns, 222);
            assert_eq!(got[1].samples_needed, 256);
            assert_eq!(got[1].sample_rate, 44_100);
            assert_eq!(got[1].tick_number, 1);
        }

        // Registration still live → the closure must NOT have dropped.
        assert_eq!(
            drop_counter.load(Ordering::SeqCst),
            0,
            "closure must not drop while the on-tick registration is live",
        );

        // Release the registration → the host fires the drop trampoline
        // exactly once, reclaiming the boxed closure.
        REGISTERED.with(|slot| {
            let trio = slot.borrow_mut().take().unwrap();
            // SAFETY: the trio's drop_user_data is the shim's drop
            // trampoline; user_data is the boxed closure it registered.
            unsafe { (trio.drop_user_data)(trio.user_data) };
        });

        assert_eq!(
            drop_counter.load(Ordering::SeqCst),
            1,
            "drop_user_data must reclaim the boxed closure exactly once \
             (0 = leak / missing Box::from_raw; 2 = double-free)",
        );
    }

    /// The real defect: `audio_clock_vtable` is ABI-optional, so
    /// `audio_clock()` can hand the shim a null vtable pointer on the
    /// reachable no-clock path. The POD getters must read as zero (not
    /// deref null -> SIGSEGV), and `on_tick` must be a no-op that STILL
    /// reclaims the boxed user closure exactly once rather than leaking it.
    /// Mental-revert: dropping either `is_null()` guard in
    /// `sample_rate`/`buffer_size` dereferences null and segfaults; dropping
    /// the `on_tick` reclaim path leaks the closure and leaves the drop
    /// counter at 0.
    #[test]
    fn null_vtable_guard_reads_zero_and_drops_closure_exactly_once() {
        // Non-null dummy handle paired with a NULL vtable — the exact shape
        // `audio_clock()` builds when no host audio clock is installed.
        let handle = std::ptr::dangling::<c_void>();
        let shim = AudioClockShim::from_ffi(handle, std::ptr::null());

        // POD getters must not deref the null vtable.
        assert_eq!(shim.sample_rate(), 0);
        assert_eq!(shim.buffer_size(), 0);

        let drop_counter = Arc::new(AtomicUsize::new(0));
        {
            let spy = ClosureDropSpy(Arc::clone(&drop_counter));
            // No host to register with: on_tick is a no-op, but the boxed
            // closure (owning `spy`) must be reclaimed here.
            shim.on_tick(move |_tick| {
                let _keep_spy_alive = &spy;
            });
        }

        assert_eq!(
            drop_counter.load(Ordering::SeqCst),
            1,
            "null-vtable on_tick must drop the boxed closure exactly once \
             (0 = leak; 2 = double-free)",
        );
    }

    /// Locks the tick trampoline's `if user_data.is_null()` guard: the host
    /// firing a tick with a null `user_data` must return without deref /
    /// panic / segfault. Mental-revert: deleting the guard casts null to a
    /// `&Box<dyn Fn(..)>` and calls through it — an immediate segfault.
    #[test]
    fn tick_trampoline_null_user_data_is_noop() {
        let repr = AudioTickContextRepr {
            timestamp_ns: 1,
            samples_needed: 512,
            sample_rate: 48_000,
            _reserved_padding: 0,
            tick_number: 0,
        };
        // SAFETY: null user_data is the exact case the guard defends.
        unsafe { audio_clock_shim_tick_trampoline(std::ptr::null_mut(), repr) };
    }

    /// Locks the drop trampoline's `if user_data.is_null()` guard: releasing
    /// a registration with a null `user_data` must return without calling
    /// `Box::from_raw(null)`. Mental-revert: deleting the guard reclaims a
    /// box from null and aborts.
    #[test]
    fn drop_trampoline_null_user_data_is_noop() {
        // SAFETY: null user_data is the exact case the guard defends.
        unsafe { audio_clock_shim_drop_trampoline(std::ptr::null_mut()) };
    }
}
