// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Internal generated processor trait - DO NOT USE DIRECTLY.

use serde_json::Value as JsonValue;
use std::future::Future;

use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::error::Result;
use crate::core::execution::ExecutionConfig;
use crate::core::processors::Config;
use crate::core::ProcessorDescriptor;

/// Internal trait implemented by the processor macro.
///
/// **DO NOT IMPLEMENT DIRECTLY** - Use the `#[streamlib::processor]` macro instead.
/// For custom processor behavior, implement [`Processor`](super::super::Processor).
pub trait GeneratedProcessor: Send + 'static {
    type Config: Config;

    /// Returns the processor name.
    fn name(&self) -> &str;

    fn from_config(config: Self::Config) -> Result<Self>
    where
        Self: Sized;

    fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()>;

    /// Update configuration at runtime (hot-reload).
    fn update_config(&mut self, _config: Self::Config) -> Result<()> {
        Ok(())
    }

    /// Apply a JSON config update at runtime.
    fn apply_config_json(&mut self, config_json: &serde_json::Value) -> Result<()>
    where
        Self: Sized,
    {
        let config: Self::Config = serde_json::from_value(config_json.clone())
            .map_err(|e| crate::core::StreamError::Config(e.to_string()))?;
        self.update_config(config)
    }

    /// Returns the execution configuration for this processor.
    fn execution_config(&self) -> ExecutionConfig {
        ExecutionConfig::default()
    }

    fn descriptor() -> Option<ProcessorDescriptor>
    where
        Self: Sized;

    /// Check if this processor has iceoryx2-based output ports.
    fn has_iceoryx2_outputs(&self) -> bool {
        false
    }

    /// Check if this processor has iceoryx2-based input ports.
    fn has_iceoryx2_inputs(&self) -> bool {
        false
    }

    /// Get the OutputWriter if this processor uses iceoryx2 outputs.
    fn get_iceoryx2_output_writer(&self) -> Option<std::sync::Arc<crate::iceoryx2::OutputWriter>> {
        None
    }

    /// Get a mutable reference to the InputMailboxes if this processor uses iceoryx2 inputs.
    fn get_iceoryx2_input_mailboxes(&mut self) -> Option<&mut crate::iceoryx2::InputMailboxes> {
        None
    }

    /// Serialize processor-specific runtime state to JSON.
    fn to_runtime_json(&self) -> JsonValue {
        JsonValue::Null
    }

    /// Get the current config as JSON.
    fn config_json(&self) -> JsonValue
    where
        Self: Sized,
    {
        JsonValue::Null
    }

    /// Generated setup hook called by runtime with privileged ctx.
    fn __generated_setup(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    /// Generated teardown hook called by runtime with privileged ctx.
    fn __generated_teardown(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    /// Generated on_pause hook — restricted ctx.
    fn __generated_on_pause(
        &mut self,
        _ctx: &RuntimeContextLimitedAccess<'_>,
    ) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    /// Generated on_resume hook — restricted ctx.
    fn __generated_on_resume(
        &mut self,
        _ctx: &RuntimeContextLimitedAccess<'_>,
    ) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    /// Called once to start a Manual mode processor. Privileged ctx.
    ///
    /// Only valid for Manual execution mode. Returns an error for other modes.
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Err(crate::core::StreamError::Runtime(
            "start() is only valid for Manual execution mode".into(),
        ))
    }

    /// Called to stop a Manual mode processor. Privileged ctx.
    ///
    /// Only valid for Manual execution mode. Returns an error for other modes.
    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Err(crate::core::StreamError::Runtime(
            "stop() is only valid for Manual execution mode".into(),
        ))
    }

    /// Returns the shared audio converter status Arc, if this processor has one.
    fn get_audio_converter_status_arc(
        &self,
    ) -> Option<std::sync::Arc<std::sync::Mutex<crate::core::utils::ProcessorAudioConverterStatus>>>
    {
        None
    }
}
