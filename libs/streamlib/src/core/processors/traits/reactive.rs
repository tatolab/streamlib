// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reactive processor trait.

use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::error::Result;
use std::future::Future;

/// Processor that reacts to input data.
///
/// Runtime calls `process()` when upstream writes to any input port.
/// Use for: transforms, filters, effects, encoders, decoders.
///
/// # Capability-typed lifecycle
///
/// Lifecycle methods receive a capability-typed context parameter:
///
/// - [`setup`](ReactiveProcessor::setup) and
///   [`teardown`](ReactiveProcessor::teardown) get
///   [`RuntimeContextFullAccess`] — privileged, allows resource allocation
///   and device-wide operations via `ctx.gpu_full_access()`.
/// - [`process`](ReactiveProcessor::process),
///   [`on_pause`](ReactiveProcessor::on_pause), and
///   [`on_resume`](ReactiveProcessor::on_resume) get
///   [`RuntimeContextLimitedAccess`] — cheap, pool-backed, non-allocating
///   operations only via `ctx.gpu_limited_access()`.
///
/// Both types are `!Clone` and borrow-scoped — the `ctx` cannot be stashed
/// past the call. Pre-reserve resources in `setup()`; use them from
/// `process()` through the limited-access ctx.
pub trait ReactiveProcessor {
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

    /// Called when input data arrives. Restricted ctx.
    fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()>;
}
