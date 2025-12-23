// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Continuous processor trait.

use crate::core::error::Result;
use crate::core::RuntimeContext;
use std::future::Future;

/// Processor that runs continuously in a loop.
///
/// Runtime calls `process()` repeatedly, with optional interval between calls.
/// Use for: generators, sources, polling, batch processing.
pub trait ContinuousProcessor {
    /// Called once when the processor starts.
    fn setup(&mut self, _ctx: RuntimeContext) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    /// Called once when the processor stops.
    fn teardown(&mut self) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    /// Called when the processor is paused.
    fn on_pause(&mut self) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    /// Called when the processor is resumed after being paused.
    fn on_resume(&mut self) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    /// Called repeatedly by the runtime in a loop.
    fn process(&mut self) -> Result<()>;
}
