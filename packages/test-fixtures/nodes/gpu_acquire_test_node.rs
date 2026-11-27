// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Integration-test fixture that exercises the GpuContextLimitedAccess
//! surface end-to-end.
//!
//! The body clones `ctx.gpu_limited_access()`, bumping the
//! `Arc<GpuContext>` refcount. Its one computed value, built from
//! `width` and `height`, acquires a `PixelBuffer` through the clone,
//! reads the cached `width`/`height`, calls `plane_base_address(0)` and
//! writes a sentinel byte through the returned pointer to prove the
//! host-allocated mapped memory is reachable, then drops the
//! `PixelBuffer` and writes "OK\n<width>x<height>\nsentinel_addr=0x<hex>"
//! or "ERR:<message>" to the file its `output_path` names so the
//! integration test can verify the round-trip. The clone drops with the
//! node.

use streamlib::sdk::context::{GpuContextLimitedAccess, RuntimeContextFullAccess};
use streamlib::sdk::error::Result;
use streamlib::sdk::nodes::{Input, computed};
use streamlib::sdk::rhi::PixelFormat;

use crate::result_written_to_its_file::the_result_written_to_its_file;

#[streamlib::sdk::node(
    execution = manual,
    input("height", description = "The height of the pixel buffer acquired, in pixels.", value = u32, default = 0),
    input("output_path", description = "The file the node writes its result to.", value = String, default = String::new()),
    input("width", description = "The width of the pixel buffer acquired, in pixels.", value = u32, default = 0),
)]
#[meta(
    description = "GPU integration test fixture — exercises acquire_pixel_buffer, plane_base_address_pixel_buffer, and the pixel-buffer lifecycle through GpuContextLimitedAccess"
)]
pub fn gpu_acquire_test(
    height: Input<u32>,
    output_path: Input<String>,
    width: Input<u32>,
    ctx: &RuntimeContextFullAccess<'_>,
) {
    let gpu = ctx.gpu_limited_access().clone();
    computed(move || {
        let outcome = acquire_a_pixel_buffer_and_write_its_plane(&gpu, *width.get(), *height.get())
            .map(|(w, h, ptr)| format!("\n{w}x{h}\nsentinel_addr=0x{:x}", ptr as usize));
        the_result_written_to_its_file("GpuAcquireTest", &output_path.get(), &outcome)
    });
}

/// Acquire a `width` × `height` pixel buffer through `gpu` and write a sentinel byte to its
/// first plane: the width and height it holds, and the plane's address.
fn acquire_a_pixel_buffer_and_write_its_plane(
    gpu: &GpuContextLimitedAccess,
    width: u32,
    height: u32,
) -> Result<(u32, u32, *mut u8)> {
    // Exercise `acquire_pixel_buffer` — paired out-param
    // tuple return.
    let (_pool_id, pixel_buffer) = gpu.acquire_pixel_buffer(width, height, PixelFormat::Bgra32)?;

    // Read the cached POD width/height (pure field reads).
    let observed_w = pixel_buffer.width;
    let observed_h = pixel_buffer.height;

    // `plane_base_address` returns a host-allocated mapped
    // pointer in the same process address space.
    let plane_ptr = pixel_buffer.plane_base_address(0);

    // If the mapping is HOST_VISIBLE, write a sentinel byte
    // to prove the mapped memory is reachable.
    if !plane_ptr.is_null() {
        // SAFETY: `plane_ptr` is the mapped base address for
        // plane 0 of a freshly-acquired HOST_VISIBLE pixel
        // buffer. The host's VMA allocator guarantees this
        // pointer is valid for at least one byte for the
        // buffer's lifetime; we own the `PixelBuffer` value
        // here so the lifetime is the duration of this call.
        unsafe {
            *plane_ptr = 0xAB;
        }
    }

    drop(pixel_buffer);

    Ok((observed_w, observed_h, plane_ptr))
}
