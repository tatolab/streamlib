// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `AudioClockVTable` — extern "C" dispatch for `SharedAudioClock` plus the
//! paired `AudioTickContextRepr` FFI-mirror struct.

use core::ffi::c_void;

/// Layout version of [`crate::AudioClockVTable`].
pub const AUDIO_CLOCK_VTABLE_LAYOUT_VERSION: u32 = 1;

/// FFI-compatible mirror of `AudioTickContext` carried into
/// extern "C" tick callbacks. Field order matches the host-side
/// `AudioTickContext` and is locked by layout-regression tests.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct AudioTickContextRepr {
    pub timestamp_ns: i64,
    pub samples_needed: u64,
    pub sample_rate: u32,
    pub _reserved_padding: u32,
    pub tick_number: u64,
}

/// Dispatch table for the host's audio clock. The cdylib obtains a
/// handle via [`crate::RuntimeContextVTable::audio_clock_handle`] and reads
/// the static vtable from [`crate::HostServices::audio_clock_vtable`].
#[repr(C)]
pub struct AudioClockVTable {
    pub layout_version: u32,
    pub _reserved_padding: u32,

    /// Returns the clock's sample rate in Hz.
    pub sample_rate: unsafe extern "C" fn(handle: *const c_void) -> u32,

    /// Returns the clock's buffer size (samples per tick).
    pub buffer_size: unsafe extern "C" fn(handle: *const c_void) -> usize,

    /// Register a tick callback. The host owns the callback registration
    /// and invokes `callback(user_data, AudioTickContextRepr)` on every
    /// tick. The `drop_user_data` fn is invoked when the registration
    /// is released (host shutdown or clock teardown). Multiple
    /// registrations are permitted; they fire in registration order.
    pub on_tick: unsafe extern "C" fn(
        handle: *const c_void,
        callback: unsafe extern "C" fn(*mut c_void, AudioTickContextRepr),
        user_data: *mut c_void,
        drop_user_data: unsafe extern "C" fn(*mut c_void),
    ),
}

unsafe impl Send for AudioClockVTable {}
unsafe impl Sync for AudioClockVTable {}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn audio_tick_context_repr_layout() {
        // 5 fields: i64 + u64 + u32 + u32 + u64 = 8+8+4+4+8 = 32 bytes
        // with 8-byte alignment from the i64/u64.
        assert_eq!(size_of::<AudioTickContextRepr>(), 32);
        assert_eq!(align_of::<AudioTickContextRepr>(), 8);
        assert_eq!(offset_of!(AudioTickContextRepr, timestamp_ns), 0);
        assert_eq!(offset_of!(AudioTickContextRepr, samples_needed), 8);
        assert_eq!(offset_of!(AudioTickContextRepr, sample_rate), 16);
        assert_eq!(offset_of!(AudioTickContextRepr, _reserved_padding), 20);
        assert_eq!(offset_of!(AudioTickContextRepr, tick_number), 24);
    }

    #[test]
    fn audio_clock_vtable_layout() {
        // 4 + 4 + 3 fn pointers = 32 bytes
        assert_eq!(size_of::<AudioClockVTable>(), 32);
        assert_eq!(align_of::<AudioClockVTable>(), 8);
        assert_eq!(offset_of!(AudioClockVTable, layout_version), 0);
        assert_eq!(offset_of!(AudioClockVTable, _reserved_padding), 4);
        assert_eq!(offset_of!(AudioClockVTable, sample_rate), 8);
        assert_eq!(offset_of!(AudioClockVTable, buffer_size), 16);
        assert_eq!(offset_of!(AudioClockVTable, on_tick), 24);
    }
}
