// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Object-safe wrapper for GeneratedProcessor - DO NOT USE DIRECTLY.

use super::GeneratedProcessor;
use crate::core::execution::ExecutionConfig;
use crate::core::runtime::BoxFuture;
use crate::core::ProcessorDescriptor;
use crate::core::{Result, RuntimeContext};

/// Object-safe version of [`GeneratedProcessor`] for dynamic dispatch.
///
/// **DO NOT USE DIRECTLY** - This is an internal implementation detail.
///
/// Uses [`BoxFuture`] for async lifecycle methods to maintain object safety.
pub trait DynGeneratedProcessor: Send + 'static {
    /// Generated setup hook called by runtime.
    ///
    /// Returns a boxed future for object safety. The future must not borrow
    /// from `ctx` - clone it if needed in async code.
    fn __generated_setup(&mut self, ctx: RuntimeContext) -> BoxFuture<'_, Result<()>>;

    /// Generated teardown hook called by runtime.
    ///
    /// Returns a boxed future for object safety.
    fn __generated_teardown(&mut self) -> BoxFuture<'_, Result<()>>;

    /// Generated on_pause hook called by runtime when processor is paused.
    ///
    /// Returns a boxed future for object safety.
    fn __generated_on_pause(&mut self) -> BoxFuture<'_, Result<()>>;

    /// Generated on_resume hook called by runtime when processor is resumed.
    ///
    /// Returns a boxed future for object safety.
    fn __generated_on_resume(&mut self) -> BoxFuture<'_, Result<()>>;

    fn process(&mut self) -> Result<()>;

    /// Called once to start a Manual mode processor.
    fn start(&mut self) -> Result<()>;

    /// Called to stop a Manual mode processor.
    fn stop(&mut self) -> Result<()>;

    fn name(&self) -> &str;
    fn descriptor(&self) -> Option<ProcessorDescriptor>;

    /// Returns the execution configuration for this processor.
    fn execution_config(&self) -> ExecutionConfig;

    /// Check if this processor has iceoryx2-based output ports.
    fn has_iceoryx2_outputs(&self) -> bool;

    /// Check if this processor has iceoryx2-based input ports.
    fn has_iceoryx2_inputs(&self) -> bool;

    /// Get a mutable reference to the OutputWriter if this processor uses iceoryx2 outputs.
    fn get_iceoryx2_output_writer(&mut self) -> Option<&mut crate::iceoryx2::OutputWriter>;

    /// Get a mutable reference to the InputMailboxes if this processor uses iceoryx2 inputs.
    fn get_iceoryx2_input_mailboxes(&mut self) -> Option<&mut crate::iceoryx2::InputMailboxes>;

    /// Apply a JSON config update at runtime.
    fn apply_config_json(&mut self, config_json: &serde_json::Value) -> crate::core::Result<()>;

    /// Serialize processor-specific runtime state to JSON.
    fn to_runtime_json(&self) -> serde_json::Value;

    /// Get the current config as JSON.
    fn config_json(&self) -> serde_json::Value;

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

/// Blanket implementation of DynGeneratedProcessor for all GeneratedProcessor types.
impl<T> DynGeneratedProcessor for T
where
    T: GeneratedProcessor,
{
    fn __generated_setup(&mut self, ctx: RuntimeContext) -> BoxFuture<'_, Result<()>> {
        Box::pin(<Self as GeneratedProcessor>::__generated_setup(self, ctx))
    }

    fn __generated_teardown(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(<Self as GeneratedProcessor>::__generated_teardown(self))
    }

    fn __generated_on_pause(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(<Self as GeneratedProcessor>::__generated_on_pause(self))
    }

    fn __generated_on_resume(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(<Self as GeneratedProcessor>::__generated_on_resume(self))
    }

    fn process(&mut self) -> Result<()> {
        <Self as GeneratedProcessor>::process(self)
    }

    fn start(&mut self) -> Result<()> {
        <Self as GeneratedProcessor>::start(self)
    }

    fn stop(&mut self) -> Result<()> {
        <Self as GeneratedProcessor>::stop(self)
    }

    fn name(&self) -> &str {
        <Self as GeneratedProcessor>::name(self)
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <T as GeneratedProcessor>::descriptor()
    }

    fn execution_config(&self) -> ExecutionConfig {
        <Self as GeneratedProcessor>::execution_config(self)
    }

    fn has_iceoryx2_outputs(&self) -> bool {
        <Self as GeneratedProcessor>::has_iceoryx2_outputs(self)
    }

    fn has_iceoryx2_inputs(&self) -> bool {
        <Self as GeneratedProcessor>::has_iceoryx2_inputs(self)
    }

    fn get_iceoryx2_output_writer(&mut self) -> Option<&mut crate::iceoryx2::OutputWriter> {
        <Self as GeneratedProcessor>::get_iceoryx2_output_writer(self)
    }

    fn get_iceoryx2_input_mailboxes(&mut self) -> Option<&mut crate::iceoryx2::InputMailboxes> {
        <Self as GeneratedProcessor>::get_iceoryx2_input_mailboxes(self)
    }

    fn apply_config_json(&mut self, config_json: &serde_json::Value) -> crate::core::Result<()> {
        <Self as GeneratedProcessor>::apply_config_json(self, config_json)
    }

    fn to_runtime_json(&self) -> serde_json::Value {
        <Self as GeneratedProcessor>::to_runtime_json(self)
    }

    fn config_json(&self) -> serde_json::Value {
        <Self as GeneratedProcessor>::config_json(self)
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
