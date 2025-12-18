// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! User-facing processor behavior trait.

use crate::core::error::Result;
use crate::core::RuntimeContext;

/// Defines the behavior for a processor.
///
/// Implement this trait on your generated processor struct to define its behavior.
/// The `#[streamlib::processor]` macro generates a module with a `Processor` struct
/// that you implement this trait on.
///
/// # Example
///
/// ```ignore
/// use streamlib::core::{Processor, RuntimeContext, Result};
///
/// #[streamlib::processor(execution = Manual, description = "My processor")]
/// pub struct MyProcessor {
///     #[streamlib::config]
///     config: MyConfig,
/// }
///
/// impl Processor for MyProcessor::Processor {
///     fn process(&mut self) -> Result<()> {
///         // Your processing logic here
///         Ok(())
///     }
///
///     // setup() and teardown() are optional - defaults do nothing
/// }
/// ```
///
/// # Lifecycle
///
/// 1. `setup()` - Called once when the processor starts
/// 2. `process()` - Called according to execution mode:
///    - `Manual`: Called once, you control timing via callbacks
///    - `Reactive`: Called when upstream writes to any input port
///    - `Continuous`: Called repeatedly in a loop
/// 3. `teardown()` - Called once when the processor stops
pub trait Processor {
    /// Called once when the processor starts.
    ///
    /// Use this for one-time initialization that requires runtime context.
    fn setup(&mut self, _ctx: &RuntimeContext) -> Result<()> {
        Ok(())
    }

    /// Called once when the processor stops.
    ///
    /// Use this for cleanup.
    fn teardown(&mut self) -> Result<()> {
        Ok(())
    }

    /// Your main processing logic.
    ///
    /// Called according to the execution mode specified in the processor attribute.
    fn process(&mut self) -> Result<()>;
}
