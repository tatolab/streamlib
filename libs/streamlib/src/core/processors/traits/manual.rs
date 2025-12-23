// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Manual processor trait.

use crate::core::error::Result;
use crate::core::RuntimeContext;
use std::future::Future;

/// Processor with manual timing control.
///
/// Runtime calls `process()` once, then you control all timing via callbacks,
/// hardware interrupts, or external schedulers.
/// Use for: audio output (hardware callbacks), display (vsync), cameras.
pub trait ManualProcessor {
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

    /// Called according to the execution mode.
    fn process(&mut self) -> Result<()>;
}
