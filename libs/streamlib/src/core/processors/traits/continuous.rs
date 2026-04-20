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
    fn setup<'a>(
        &'a mut self,
        _ctx: &'a RuntimeContextFullAccess<'a>,
    ) -> impl Future<Output = Result<()>> + Send + 'a {
        std::future::ready(Ok(()))
    }

    /// Called once when the processor stops. Privileged ctx.
    fn teardown<'a>(
        &'a mut self,
        _ctx: &'a RuntimeContextFullAccess<'a>,
    ) -> impl Future<Output = Result<()>> + Send + 'a {
        std::future::ready(Ok(()))
    }

    /// Called when the processor is paused. Restricted ctx.
    fn on_pause<'a>(
        &'a mut self,
        _ctx: &'a RuntimeContextLimitedAccess<'a>,
    ) -> impl Future<Output = Result<()>> + Send + 'a {
        std::future::ready(Ok(()))
    }

    /// Called when the processor is resumed after being paused. Restricted ctx.
    fn on_resume<'a>(
        &'a mut self,
        _ctx: &'a RuntimeContextLimitedAccess<'a>,
    ) -> impl Future<Output = Result<()>> + Send + 'a {
        std::future::ready(Ok(()))
    }

    /// Called repeatedly by the runtime in a loop. Restricted ctx.
    fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()>;
}
