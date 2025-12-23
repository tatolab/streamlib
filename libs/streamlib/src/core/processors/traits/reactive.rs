// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reactive processor trait.

use crate::core::error::Result;
use crate::core::RuntimeContext;
use std::future::Future;

/// Processor that reacts to input data.
///
/// Runtime calls `process()` when upstream writes to any input port.
/// Use for: transforms, filters, effects, encoders, decoders.
pub trait ReactiveProcessor {
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

    /// Called when input data arrives.
    fn process(&mut self) -> Result<()>;
}
