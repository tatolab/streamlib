// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Continuous processor trait.

use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::error::Result;
use std::future::Future;

/// Processor that runs continuously in a loop.
///
/// Runtime calls `process()` repeatedly, with optional interval between calls.
/// Use for: generators, sources, polling, batch processing.
///
/// See [`ReactiveProcessor`](super::reactive::ReactiveProcessor) for the
/// capability-typed lifecycle contract — the same rules apply here.
pub trait ContinuousProcessor {
    /// Called once when the processor starts. Privileged ctx.
    fn setup(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    /// Called once when the processor stops. Privileged ctx.
    fn teardown(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    /// Called when the processor is paused. Restricted ctx.
    fn on_pause(
        &mut self,
        _ctx: &RuntimeContextLimitedAccess<'_>,
    ) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    /// Called when the processor is resumed after being paused. Restricted ctx.
    fn on_resume(
        &mut self,
        _ctx: &RuntimeContextLimitedAccess<'_>,
    ) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    /// Called repeatedly by the runtime in a loop. Restricted ctx.
    fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()>;
}
