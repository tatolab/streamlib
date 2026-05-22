// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Phase C3 (#903) dlopen-cdylib escalate smoke test fixture.
//!
//! Proves the scope-token machinery fires end-to-end through the FFI
//! boundary. The processor's `start()` runs:
//!
//! ```ignore
//! ctx.gpu_limited_access().escalate(|_full| Ok(()))
//! ```
//!
//! which exercises:
//!   1. `escalate_begin` vtable callback (host mints a scope token,
//!      enters the escalate gate, registers the per-scope Arc).
//!   2. Cdylib-side `GpuContextFullAccess::from_scope_token`
//!      construction (vtable-dispatched path).
//!   3. Closure runs (empty body — just verifies the FullAccess
//!      borrow is reachable).
//!   4. Cdylib-side `GpuContextFullAccess::drop` short-circuits for
//!      the `HandleKind::ScopeToken` case (cleanup happens in
//!      `escalate_end`, not Drop).
//!   5. `escalate_end` vtable callback (host removes the scope,
//!      releases the gate, runs `wait_device_idle`).
//!
//! What this test does NOT cover: cdylib-side dispatch on
//! `GpuContextFullAccess` methods (`create_compute_kernel`, etc.) —
//! those still call `host_inner()` and panic in cdylib mode. That
//! gap is the work of Phase D (#906) and Phase E (#907). The richer
//! "create kernel + dispatch + verify CPU reference" test originally
//! specced for #903 lives in #907.
//!
//! Output format:
//!   - "OK" — both `escalate_begin` and `escalate_end` succeeded and
//!     the closure ran.
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

        // Escalate with an empty closure. In cdylib mode this routes
        // through `escalate_via_vtable`: vtable.escalate_begin → mint
        // scope_token → construct GpuContextFullAccess via
        // from_scope_token → run closure → vtable.escalate_end. A
        // panic at any FFI boundary surfaces as a panic in the
        // closure or a non-zero escalate return; both produce an
        // "ERR:" line.
        let result: Result<()> =
            ctx.gpu_limited_access().escalate(|_full| Ok(()));

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
