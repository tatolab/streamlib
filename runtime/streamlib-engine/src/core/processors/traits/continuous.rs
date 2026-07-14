// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Continuous processor trait.

use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::error::Result;

/// Processor that runs continuously in a loop.
///
/// Runtime calls `process()` repeatedly, with optional interval between calls.
/// Use for: generators, sources, polling, batch processing.
///
/// See [`ReactiveProcessor`](super::reactive::ReactiveProcessor) for the
/// capability-typed lifecycle contract — the same rules apply here.
///
/// All lifecycle methods are synchronous at the trait surface. Plugins that
/// need async work in lifecycle methods construct their own async runtime
/// (tokio, smol, etc.) in `setup`, stash a handle on `self`, and use it
/// via `block_on` from within the sync method bodies. The host does NOT
/// expose an async runtime — see issue #885 for the rationale.
pub trait ContinuousProcessor {
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

    /// Called repeatedly by the runtime in a loop. Restricted ctx.
    fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()>;
}
