// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The pixel pattern the raw-Mach surface-share test's engine writes and its
//! helper verifies, shared so the two sides cannot drift apart.

use objc2_io_surface::{IOSurfaceLockOptions, IOSurfaceRef};

/// The byte the engine writes at `index` of a shared surface.
pub fn engine_pattern_byte(index: usize) -> u8 {
    (index.wrapping_mul(31).wrapping_add(7)) as u8
}

/// Run `touch` over the surface's bytes under its lock.
pub fn with_the_surface_bytes<R>(
    iosurface: &IOSurfaceRef,
    touch: impl FnOnce(&mut [u8]) -> R,
) -> R {
    let locked = unsafe { iosurface.lock(IOSurfaceLockOptions::empty(), std::ptr::null_mut()) };
    assert_eq!(locked, 0, "IOSurfaceLock");
    let byte_len = iosurface.bytes_per_row() * iosurface.height();
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(iosurface.base_address().as_ptr().cast::<u8>(), byte_len)
    };
    let touched = touch(bytes);
    unsafe { iosurface.unlock(IOSurfaceLockOptions::empty(), std::ptr::null_mut()) };
    touched
}
