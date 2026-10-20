// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The handle count of the `Arc` an opaque handle leaked with `Arc::into_raw`.

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::sync::Arc;

/// The strong count of the `Arc<Inner>` `handle` leaked; 0 for a null handle.
///
/// # Safety
///
/// A non-null `handle` came from `Arc::<Inner>::into_raw`, and the count it
/// leaked is still held.
pub(crate) unsafe fn strong_count_of_leaked_arc<Inner>(handle: *const c_void) -> usize {
    if handle.is_null() {
        return 0;
    }
    // SAFETY: the caller's contract; `ManuallyDrop` keeps the leaked count, so
    // reading it leaves the count where it was.
    let leaked = ManuallyDrop::new(unsafe { Arc::from_raw(handle as *const Inner) });
    Arc::strong_count(&leaked)
}
