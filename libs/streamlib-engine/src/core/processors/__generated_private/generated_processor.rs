// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Internal generated processor trait - DO NOT USE DIRECTLY.

use serde_json::Value as JsonValue;

use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::error::Result;
use crate::core::execution::ExecutionConfig;
use crate::core::processors::Config;
use crate::core::ProcessorDescriptor;

/// Internal trait implemented by the processor macro.
///
/// **DO NOT IMPLEMENT DIRECTLY** - Use the `#[streamlib::sdk::processor]` macro instead.
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
            .map_err(|e| crate::core::Error::Config(e.to_string()))?;
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

    /// Install host-allocated iceoryx2 resources on this processor
    /// (issue #894 host-allocates flip).
    ///
    /// Called by the host once after `from_config` returns and
    /// before any connections are wired. Default impl is a no-op
    /// for processors that declare no input/output ports; the macro
    /// emits an override that writes the β-shapes into `self.outputs`
    /// / `self.inputs` for processors that do.
    fn set_iceoryx2_resources(
        &mut self,
        _output_writer: Option<crate::iceoryx2::OutputWriter>,
        _input_mailboxes: Option<crate::iceoryx2::InputMailboxes>,
    ) -> Result<()> {
        Ok(())
    }

    /// Borrow the (now host-side) `OutputWriterInner` Arc the
    /// processor's β-shape is wired to, if any. Default `None`;
    /// the macro emits an override for processors with outputs.
    ///
    /// Used by the host's connection-wiring path to mutate the
    /// inner directly (add_connection, etc.) without crossing
    /// the cdylib boundary. The returned Arc is cloned from the
    /// β-shape's stored Arc, so it's safe to retain.
    fn iceoryx2_output_writer_inner(
        &self,
    ) -> Option<std::sync::Arc<crate::iceoryx2::OutputWriterInner>> {
        None
    }

    /// Borrow the (now host-side) `InputMailboxesInner` Arc the
    /// processor's β-shape is wired to, if any. Default `None`;
    /// the macro emits an override for processors with inputs.
    fn iceoryx2_input_mailboxes_inner(
        &self,
    ) -> Option<std::sync::Arc<crate::iceoryx2::InputMailboxesInner>> {
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
    /// Sync per the Phase B ABI; plugins that want async setup work do
    /// their own `block_on` against a self-owned runtime.
    fn __generated_setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Generated teardown hook called by runtime with privileged ctx.
    fn __generated_teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Generated on_pause hook — restricted ctx.
    fn __generated_on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Generated on_resume hook — restricted ctx.
    fn __generated_on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Called once to start a Manual mode processor. Privileged ctx.
    ///
    /// Only valid for Manual execution mode. Returns an error for other modes.
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Err(crate::core::Error::Runtime(
            "start() is only valid for Manual execution mode".into(),
        ))
    }

    /// Called to stop a Manual mode processor. Privileged ctx.
    ///
    /// Only valid for Manual execution mode. Returns an error for other modes.
    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Err(crate::core::Error::Runtime(
            "stop() is only valid for Manual execution mode".into(),
        ))
    }
}
