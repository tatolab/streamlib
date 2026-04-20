// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Manual processor trait.

use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::error::Result;
use std::future::Future;

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
pub trait ManualProcessor {
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
