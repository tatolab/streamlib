// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Phase C3 + Phase D dlopen-cdylib escalate smoke test fixture.
//!
//! Proves the scope-token machinery fires end-to-end through the FFI
//! boundary AND that representative `GpuContextFullAccess` vtable
//! slots — both Phase D Bucket B (new FullAccess slots) and Bucket C
//! (Option B Limited-vtable inheritance) — dispatch correctly from
//! cdylib code. The processor's `start()` runs an escalate scope that
//! exercises:
//!
//!   1. `escalate_begin` vtable callback (host mints a scope token,
//!      enters the escalate gate, registers the per-scope Arc).
//!   2. Cdylib-side `GpuContextFullAccess::from_scope_token`
//!      construction (vtable-dispatched path).
//!   3. Inside the closure:
//!      - `full.wait_device_idle()` — Phase D Bucket B (new FullAccess
//!        vtable slot). Validates the scope-token resolution path via
//!        `with_full_scope_or_err`.
//!      - `full.acquire_pixel_buffer(...)` — Phase D Bucket C
//!        (Option B inherited LimitedAccess vtable dispatch).
//!        Returns a `PixelBuffer` β-shape through the FFI.
//!      - `full.acquire_output_texture(...)` — Phase D Bucket B
//!        (new FullAccess vtable slot returning `(String, Texture)`).
//!      - `full.register_texture_with_layout(...)` — Phase D Bucket C
//!        (Option B inherited dispatch). Per #906 exit criterion #4
//!        explicitly names this method.
//!      **This is the load-bearing Phase D end-to-end coverage** — if
//!      any FullAccess method dispatch regresses, this test fails.
//!   4. Cdylib-side `GpuContextFullAccess::drop` short-circuits for
//!      the `HandleKind::ScopeToken` case (cleanup happens in
//!      `escalate_end`, not Drop).
//!   5. `escalate_end` vtable callback (host removes the scope,
//!      releases the gate, runs `wait_device_idle`).
//!
//! What this test does NOT cover: per-method dispatch on the kernel /
//! acceleration-structure / command-recorder *handles* returned by
//! Phase D's `create_*` and `build_*` methods (Phase E #907 wires that).
//! Phase D itself ensures the handles can be obtained from cdylib;
//! using them is Phase E's scope.
//!
//! Output format:
//!   - "OK" — escalate round-trip + all four method calls succeeded.
//!   - "ERR:<message>" — any step failed.

use streamlib::sdk::context::{
    RuntimeContextFullAccess, RuntimeContextLimitedAccess,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ManualProcessor;
use streamlib::sdk::rhi::{PixelFormat, TextureFormat};
#[cfg(target_os = "linux")]
use streamlib::engine_internal::sdk::rhi::VulkanLayout;

#[streamlib::sdk::processor("EscalateSmokeTestProcessor")]
pub struct EscalateSmokeTest {}

impl ManualProcessor for EscalateSmokeTest::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let output_path = self.config.output_path.clone();

        // Escalate and exercise both Phase D vtable dispatch paths
        // end-to-end. In cdylib mode this routes through
        // `escalate_via_vtable`: vtable.escalate_begin → mint
        // scope_token → construct GpuContextFullAccess via
        // from_scope_token → four FullAccess methods dispatch through
        // the FullAccess vtable / inherited LimitedAccess vtable →
        // vtable.escalate_end. A panic at any FFI boundary surfaces
        // as a panic in the closure or a non-zero escalate return;
        // both produce an "ERR:" line.
        //
        // Coverage:
        //   - wait_device_idle: Phase D Bucket B (new FullAccess slot).
        //   - acquire_pixel_buffer: Phase D Bucket C (inherited
        //     LimitedAccess vtable via Option B).
        //   - acquire_output_texture: Phase D Bucket B (new FullAccess
        //     slot). Used to mint a Texture for the next call.
        //   - register_texture_with_layout: Phase D Bucket C (inherited
        //     LimitedAccess vtable). Per #906 exit criterion #4 this
        //     method is explicitly required.
        let result: Result<()> = ctx.gpu_limited_access().escalate(|full| {
            // Bucket B — new FullAccess vtable slot.
            full.wait_device_idle()?;

            // Bucket C — inherited LimitedAccess dispatch.
            let (_pool_id, _pb) =
                full.acquire_pixel_buffer(64, 64, PixelFormat::Rgba32)?;

            // Bucket B — new FullAccess vtable slot returning a Texture
            // we can hand to register_texture_with_layout below.
            let (id, texture) =
                full.acquire_output_texture(64, 64, TextureFormat::Rgba8Unorm)?;

            // Bucket C — inherited LimitedAccess dispatch with an
            // explicit non-UNDEFINED layout to exercise the layout
            // argument path. Picks SHADER_READ_ONLY_OPTIMAL as a
            // representative non-default value.
            #[cfg(target_os = "linux")]
            full.register_texture_with_layout(
                &id,
                texture,
                VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            );
            #[cfg(not(target_os = "linux"))]
            {
                // VulkanLayout doesn't exist on non-Linux; on those
                // platforms the test still exercises the other three
                // methods.
                drop(id);
                drop(texture);
            }

            Ok(())
        });

        let line = match result {
            Ok(()) => "OK".to_string(),
            Err(e) => format!("ERR:{e}"),
        };
        std::fs::write(&output_path, &line).map_err(|e| {
            Error::Runtime(format!("EscalateSmokeTest: write {output_path}: {e}"))
        })?;
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }
}
