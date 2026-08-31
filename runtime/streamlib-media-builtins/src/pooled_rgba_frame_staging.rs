// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Staging a CPU-side RGBA picture into a pooled pixel buffer, so its pool id
//! can be published as a frame's `surface_id`.
//!
//! One site, because it is one `unsafe` block: every producer whose pixels
//! arrive on the CPU — a decoder's readback, a file-replaying fixture source —
//! acquires an `Rgba32` buffer and copies into plane 0, and reasoning about
//! that copy in each of them separately is how one of them ends up with a
//! guard the others have.
//!
//! The RHI exposes no per-row stride: `plane_base_address` / `plane_size` are
//! the whole surface, so plane 0 is tightly packed by construction and a
//! source of `width * height * 4` bytes maps onto it one-to-one.

use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::rhi::PixelBuffer;

/// Copy `rgba_pixels` into plane 0 of an already-acquired `Rgba32`
/// `pixel_buffer`, refusing by name when either side is too small to hold a
/// `width` × `height` picture rather than copying a partial one.
pub fn stage_tightly_packed_rgba_into_pooled_pixel_buffer(
    pixel_buffer: &PixelBuffer,
    rgba_pixels: &[u8],
    width: u32,
    height: u32,
) -> Result<()> {
    let rgba_byte_count = (width as usize) * (height as usize) * 4;
    let plane_pointer = pixel_buffer.plane_base_address(0);
    let plane_size = pixel_buffer.plane_size(0) as usize;
    if plane_pointer.is_null()
        || plane_size < rgba_byte_count
        || rgba_pixels.len() < rgba_byte_count
    {
        return Err(Error::Runtime(format!(
            "cannot stage a {width}x{height} RGBA picture: pixel-buffer plane pointer null: {}, \
             plane {plane_size} bytes, source {} bytes, needs {rgba_byte_count}",
            plane_pointer.is_null(),
            rgba_pixels.len()
        )));
    }
    // SAFETY: `plane_pointer` is the mapped host-visible base of plane 0 of an
    // `Rgba32` pixel buffer, valid for `plane_size` bytes and checked above to
    // be at least `rgba_byte_count`; the source is a distinct caller-owned
    // slice of at least that length, so the regions cannot overlap; and
    // `copy_nonoverlapping::<u8>` needs no alignment.
    unsafe {
        std::ptr::copy_nonoverlapping(rgba_pixels.as_ptr(), plane_pointer, rgba_byte_count);
    }
    Ok(())
}
