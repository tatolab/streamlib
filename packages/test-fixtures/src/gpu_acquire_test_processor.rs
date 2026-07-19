// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integration-test fixture: a dlopen'd processor that exercises the
//! `GpuContextLimitedAccessVTable` callbacks end-to-end from cdylib
//! code.
//!
//! Lifecycle:
//!   1. `setup()` — clones `ctx.gpu_limited_access()` and stashes it.
//!      Exercises `clone_handle` (Arc refcount bump on
//!      `Arc<GpuContext>`).
//!   2. `start()` — acquires a `PixelBuffer` via the stashed
//!      `GpuContextLimitedAccess::acquire_pixel_buffer`. Exercises
//!      `acquire_pixel_buffer` (paired-out-param tuple return). Reads
//!      the cached `width`/`height` (POD; no plugin ABI dispatch).
//!      Calls `plane_base_address(0)` — exercises
//!      `plane_base_address_pixel_buffer`. Writes a sentinel byte
//!      through the returned pointer to prove the host-allocated
//!      mapped memory is reachable from cdylib code. Drops the
//!      `PixelBuffer` — exercises `drop_pixel_buffer`.
//!   3. Writes "OK\n<width>x<height>\nsentinel_addr=0x<hex>" or
//!      "ERR:<message>" to the configured `output_path` so the
//!      integration test can verify the round-trip.
//!   4. `teardown()` — drops the stashed `GpuContextLimitedAccess`,
//!      exercising `drop_handle`.

use streamlib::sdk::context::{
    GpuContextLimitedAccess, RuntimeContextFullAccess, RuntimeContextLimitedAccess,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ManualProcessor;
use streamlib::sdk::rhi::PixelFormat;

#[streamlib::sdk::processor(
    "@tatolab/test-fixtures/GpuAcquireTestProcessor@1.0.0",
    execution = manual,
    config = crate::_generated_::GpuAcquireTestProcessorConfig,
)]
pub struct GpuAcquireTest {
    gpu: Option<GpuContextLimitedAccess>,
}

impl ManualProcessor for GpuAcquireTest::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        // Clone the GpuContextLimitedAccess across the plugin ABI.
        // The `Clone` impl dispatches through the
        // `clone_handle` vtable callback (Arc refcount bump on
        // `Arc<GpuContext>`); dropping the clone in `teardown()`
        // fires `drop_handle`. Both refcount ops run in host-compiled
        // code regardless of caller plugin.
        self.gpu = Some(ctx.gpu_limited_access().clone());
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let output_path = self.config.output_path.clone();
        let width = self.config.width;
        let height = self.config.height;

        let result: Result<(u32, u32, *mut u8)> = (|| {
            let gpu = self
                .gpu
                .as_ref()
                .ok_or_else(|| Error::Runtime("GpuAcquireTest: setup() didn't stash gpu".into()))?;

            // Exercise `acquire_pixel_buffer` — paired out-param
            // tuple return.
            let (_pool_id, pixel_buffer) =
                gpu.acquire_pixel_buffer(width, height, PixelFormat::Bgra32)?;

            // Read the cached POD width/height (pure field reads,
            // no plugin ABI dispatch).
            let observed_w = pixel_buffer.width;
            let observed_h = pixel_buffer.height;

            // Exercise `plane_base_address_pixel_buffer`. Host
            // returns a host-allocated mapped pointer — same process
            // address space, so the cdylib can deref it.
            let plane_ptr = pixel_buffer.plane_base_address(0);

            // If the mapping is HOST_VISIBLE, write a sentinel byte
            // to prove cdylib→host memory access works.
            if !plane_ptr.is_null() {
                // SAFETY: `plane_ptr` is the mapped base address for
                // plane 0 of a freshly-acquired HOST_VISIBLE pixel
                // buffer. The host's VMA allocator guarantees this
                // pointer is valid for at least one byte for the
                // buffer's lifetime; we own the `PixelBuffer` value
                // here so the lifetime is the duration of `start()`.
                unsafe {
                    *plane_ptr = 0xAB;
                }
            }

            // Drop the PixelBuffer — exercises `drop_pixel_buffer`.
            // The pool slot returns to the pool host-side.
            drop(pixel_buffer);

            Ok((observed_w, observed_h, plane_ptr))
        })();

        let line = match result {
            Ok((w, h, ptr)) => format!("OK\n{w}x{h}\nsentinel_addr=0x{:x}", ptr as usize),
            Err(e) => format!("ERR:{e}"),
        };
        std::fs::write(&output_path, &line)
            .map_err(|e| Error::Runtime(format!("GpuAcquireTest: write {output_path}: {e}")))?;
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        // Drop the stashed GpuContextLimitedAccess — exercises
        // `drop_handle` (Arc refcount decrement).
        self.gpu.take();
        Ok(())
    }

    fn on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }
}
