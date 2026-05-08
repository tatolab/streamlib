// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Object-safe wrapper for GeneratedProcessor - DO NOT USE DIRECTLY.

use super::GeneratedProcessor;
use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::execution::ExecutionConfig;
use crate::core::runtime::BoxFuture;
use crate::core::ProcessorDescriptor;
use crate::core::Result;

/// Object-safe version of [`GeneratedProcessor`] for dynamic dispatch.
///
/// **DO NOT USE DIRECTLY** - This is an internal implementation detail.
///
/// Uses [`BoxFuture`] for async lifecycle methods to maintain object safety.
pub trait DynGeneratedProcessor: Send + 'static {
    /// Generated setup hook called by runtime with privileged ctx.
    fn __generated_setup<'a>(
        &'a mut self,
        ctx: &'a RuntimeContextFullAccess<'a>,
    ) -> BoxFuture<'a, Result<()>>;

    /// Generated teardown hook called by runtime with privileged ctx.
    fn __generated_teardown<'a>(
        &'a mut self,
        ctx: &'a RuntimeContextFullAccess<'a>,
    ) -> BoxFuture<'a, Result<()>>;

    /// Generated on_pause hook — restricted ctx.
    fn __generated_on_pause<'a>(
        &'a mut self,
        ctx: &'a RuntimeContextLimitedAccess<'a>,
    ) -> BoxFuture<'a, Result<()>>;

    /// Generated on_resume hook — restricted ctx.
    fn __generated_on_resume<'a>(
        &'a mut self,
        ctx: &'a RuntimeContextLimitedAccess<'a>,
    ) -> BoxFuture<'a, Result<()>>;

    fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()>;

    /// Called once to start a Manual mode processor. Privileged ctx.
    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()>;

    /// Called to stop a Manual mode processor. Privileged ctx.
    fn stop(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()>;

    fn name(&self) -> &str;
    fn descriptor(&self) -> Option<ProcessorDescriptor>;

    /// Returns the execution configuration for this processor.
    fn execution_config(&self) -> ExecutionConfig;

    /// Check if this processor has iceoryx2-based output ports.
    fn has_iceoryx2_outputs(&self) -> bool;

    /// Check if this processor has iceoryx2-based input ports.
    fn has_iceoryx2_inputs(&self) -> bool;

    /// Get the OutputWriter if this processor uses iceoryx2 outputs.
    fn get_iceoryx2_output_writer(&self) -> Option<std::sync::Arc<crate::iceoryx2::OutputWriter>>;

    /// Get a mutable reference to the InputMailboxes if this processor uses iceoryx2 inputs.
    fn get_iceoryx2_input_mailboxes(&mut self) -> Option<&mut crate::iceoryx2::InputMailboxes>;

    /// Apply a JSON config update at runtime.
    fn apply_config_json(&mut self, config_json: &serde_json::Value) -> crate::core::Result<()>;

    /// Serialize processor-specific runtime state to JSON.
    fn to_runtime_json(&self) -> serde_json::Value;

    /// Get the current config as JSON.
    fn config_json(&self) -> serde_json::Value;

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;

    /// Returns the shared audio converter status Arc, if this processor has one.
    fn get_audio_converter_status_arc(
        &self,
    ) -> Option<std::sync::Arc<std::sync::Mutex<crate::core::utils::ProcessorAudioConverterStatus>>>;
}

/// Blanket implementation of DynGeneratedProcessor for all GeneratedProcessor types.
impl<T> DynGeneratedProcessor for T
where
    T: GeneratedProcessor,
{
    fn __generated_setup<'a>(
        &'a mut self,
        ctx: &'a RuntimeContextFullAccess<'a>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(<Self as GeneratedProcessor>::__generated_setup(self, ctx))
    }

    fn __generated_teardown<'a>(
        &'a mut self,
        ctx: &'a RuntimeContextFullAccess<'a>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(<Self as GeneratedProcessor>::__generated_teardown(self, ctx))
    }

    fn __generated_on_pause<'a>(
        &'a mut self,
        ctx: &'a RuntimeContextLimitedAccess<'a>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(<Self as GeneratedProcessor>::__generated_on_pause(self, ctx))
    }

    fn __generated_on_resume<'a>(
        &'a mut self,
        ctx: &'a RuntimeContextLimitedAccess<'a>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(<Self as GeneratedProcessor>::__generated_on_resume(self, ctx))
    }

    fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        <Self as GeneratedProcessor>::process(self, ctx)
    }

    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        <Self as GeneratedProcessor>::start(self, ctx)
    }

    fn stop(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        <Self as GeneratedProcessor>::stop(self, ctx)
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

    fn get_iceoryx2_output_writer(&self) -> Option<std::sync::Arc<crate::iceoryx2::OutputWriter>> {
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

    fn get_audio_converter_status_arc(
        &self,
    ) -> Option<std::sync::Arc<std::sync::Mutex<crate::core::utils::ProcessorAudioConverterStatus>>>
    {
        <Self as GeneratedProcessor>::get_audio_converter_status_arc(self)
    }
}
