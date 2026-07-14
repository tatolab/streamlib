// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Manual processor trait.

use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::error::Result;

/// Processor with manual timing control.
///
/// Runtime calls `start()` once, then you control all timing via callbacks,
/// hardware interrupts, or external schedulers.
/// Use for: audio output (hardware callbacks), display (vsync), cameras.
///
/// # Capability-typed lifecycle
///
/// `setup`, `teardown`, `start`, and `stop` are all resource-lifecycle
/// methods and receive [`RuntimeContextFullAccess`] — privileged, allows
/// resource allocation. `on_pause` and `on_resume` receive
/// [`RuntimeContextLimitedAccess`] — hot-path-safe.
///
/// All lifecycle methods are synchronous at the trait surface. Plugins that
/// need async work in lifecycle methods construct their own async runtime
/// (tokio, smol, etc.) in `setup`, stash a handle on `self`, and use it
/// via `block_on` from within the sync method bodies. The host does NOT
/// expose an async runtime — see issue #885 for the rationale.
pub trait ManualProcessor {
    /// Called once when the processor starts. Privileged ctx.
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Called once when the processor stops. Privileged ctx.
    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Called when the processor is paused. Restricted ctx.
    fn on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Called when the processor is resumed after being paused. Restricted ctx.
    fn on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Called once to start the processor. Privileged ctx.
    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()>;

    /// Called when the processor should stop. Privileged ctx.
    ///
    /// This is called before teardown when the runtime shuts down or the
    /// processor is removed. Use this to stop internal threads, callbacks,
    /// or processing loops started by `start()`.
    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}
