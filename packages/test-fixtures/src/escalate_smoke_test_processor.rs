// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Phase C3 + Phase D dlopen-cdylib escalate smoke test fixture.
//!
//! Proves the scope-token machinery fires end-to-end through the FFI
//! boundary AND that at least one `GpuContextFullAccess` vtable slot
//! dispatches correctly from cdylib code. The processor's `start()` runs:
//!
//! ```ignore
//! ctx.gpu_limited_access().escalate(|full| full.wait_device_idle())
//! ```
//!
//! which exercises:
//!   1. `escalate_begin` vtable callback (host mints a scope token,
//!      enters the escalate gate, registers the per-scope Arc).
//!   2. Cdylib-side `GpuContextFullAccess::from_scope_token`
//!      construction (vtable-dispatched path).
//!   3. The closure body — Phase D's `full.wait_device_idle()` —
//!      dispatches through the FullAccess vtable's `wait_device_idle`
//!      slot. The host callback validates the scope token via
//!      `with_full_scope_or_err` and runs `gpu.wait_device_idle()` on
//!      the bound `Arc<GpuContext>`. **This is the load-bearing Phase
//!      D end-to-end coverage** — if any FullAccess method dispatch
//!      regresses, this test fails.
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
//!   - "OK" — full escalate round-trip + `wait_device_idle` succeeded.
//!   - "ERR:<message>" — any step failed.

use streamlib::sdk::context::{
    RuntimeContextFullAccess, RuntimeContextLimitedAccess,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ManualProcessor;

#[streamlib::sdk::processor("EscalateSmokeTestProcessor")]
pub struct EscalateSmokeTest {}

impl ManualProcessor for EscalateSmokeTest::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let output_path = self.config.output_path.clone();

        // Escalate and exercise one FullAccess vtable slot end-to-end.
        // In cdylib mode this routes through `escalate_via_vtable`:
        // vtable.escalate_begin → mint scope_token → construct
        // GpuContextFullAccess via from_scope_token →
        // full.wait_device_idle() dispatches through the FullAccess
        // vtable → vtable.escalate_end. A panic at any FFI boundary
        // surfaces as a panic in the closure or a non-zero escalate
        // return; both produce an "ERR:" line.
        //
        // `wait_device_idle` is the cheapest Phase D vtable slot
        // (parameterless, no allocations) but exercises the full
        // `with_full_scope_or_err` scope-resolution path — so if the
        // Phase D wiring regresses (cdylib wrapper calls host_inner,
        // scope-token registry breaks, etc.), this test fails.
        let result: Result<()> = ctx
            .gpu_limited_access()
            .escalate(|full| full.wait_device_idle());

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
