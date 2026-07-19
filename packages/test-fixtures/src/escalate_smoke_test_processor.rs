// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Phase C3 + Phase D dlopen-cdylib escalate smoke test fixture.
//!
//! Proves the scope-token machinery fires end-to-end through the
//! plugin ABI AND that representative `GpuContextFullAccess` vtable
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
//!        Returns a `PixelBuffer` PluginAbiObject through the plugin ABI.
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

#[cfg(target_os = "linux")]
use streamlib::engine_internal::sdk::rhi::VulkanLayout;
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ManualProcessor;
use streamlib::sdk::rhi::{PixelFormat, TextureFormat};

#[streamlib::sdk::processor(
    "@tatolab/test-fixtures/EscalateSmokeTestProcessor@1.0.0",
    execution = manual,
    config = crate::_generated_::EscalateSmokeTestProcessorConfig,
)]
pub struct EscalateSmokeTest {}

impl ManualProcessor for EscalateSmokeTest::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let output_path = self.config.output_path.clone();

        // Manual-mode start() takes FullAccess directly. The engine's
        // `ProcessorInstance::start` wraps cdylib-resident lifecycle
        // dispatch in `with_cdylib_scope` (#1075), so
        // `ctx.gpu_full_access()` is `ScopeToken`-flavored and
        // dispatches through the FullAccess vtable transparently —
        // same coverage as the pre-#1075 `escalate(|full| ...)` path,
        // just exercised via the lifecycle wrap instead of the
        // explicit escalate primitive.
        //
        // Coverage (same as pre-#1075):
        //   - wait_device_idle: Phase D Bucket B (FullAccess vtable slot).
        //   - acquire_pixel_buffer: Phase D Bucket C (inherited
        //     LimitedAccess vtable via Option B).
        //   - acquire_output_texture: Phase D Bucket B.
        //   - register_texture_with_layout: Phase D Bucket C.
        let full = ctx.gpu_full_access();
        let result: Result<()> = (|| -> Result<()> {
            // Bucket B — FullAccess vtable slot.
            full.wait_device_idle()?;

            // Bucket C — inherited LimitedAccess dispatch.
            let (_pool_id, _pb) = full.acquire_pixel_buffer(64, 64, PixelFormat::Rgba32)?;

            // Bucket B — FullAccess vtable slot returning a Texture
            // we can hand to register_texture_with_layout below.
            let (id, texture) = full.acquire_output_texture(64, 64, TextureFormat::Rgba8Unorm)?;

            // Bucket C — inherited LimitedAccess dispatch with an
            // explicit non-UNDEFINED layout to exercise the layout
            // argument path. Picks SHADER_READ_ONLY_OPTIMAL as a
            // representative non-default value.
            #[cfg(target_os = "linux")]
            full.register_texture_with_layout(&id, texture, VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
            #[cfg(not(target_os = "linux"))]
            {
                // VulkanLayout doesn't exist on non-Linux; on those
                // platforms the test still exercises the other three
                // methods.
                drop(id);
                drop(texture);
            }

            Ok(())
        })();

        let line = match result {
            Ok(()) => "OK".to_string(),
            Err(e) => format!("ERR:{e}"),
        };
        std::fs::write(&output_path, &line)
            .map_err(|e| Error::Runtime(format!("EscalateSmokeTest: write {output_path}: {e}")))?;
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
