// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::Value as JsonValue;

use crate::core::NodeDescriptor;
use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::error::Result;
use crate::core::execution::ExecutionConfig;
use crate::core::nodes::Config;

pub trait GeneratedNode: Send + 'static {
    type Config: Config;

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

    fn execution_config(&self) -> ExecutionConfig {
        ExecutionConfig::default()
    }

    fn descriptor() -> Option<NodeDescriptor>
    where
        Self: Sized;

    fn has_iceoryx2_outputs(&self) -> bool {
        false
    }

    fn has_iceoryx2_inputs(&self) -> bool {
        false
    }

    fn set_iceoryx2_resources(
        &mut self,
        _output_writer: Option<crate::iceoryx2::OutputWriter>,
        _input_mailboxes: Option<crate::iceoryx2::InputMailboxes>,
    ) -> Result<()> {
        Ok(())
    }

    fn iceoryx2_output_writer_inner(
        &self,
    ) -> Option<std::sync::Arc<crate::iceoryx2::OutputWriterInner>> {
        None
    }

    fn iceoryx2_input_mailboxes_inner(
        &self,
    ) -> Option<std::sync::Arc<crate::iceoryx2::InputMailboxesInner>> {
        None
    }

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

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Err(crate::core::Error::Runtime(
            "start() is only valid for Manual execution mode".into(),
        ))
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Err(crate::core::Error::Runtime(
            "stop() is only valid for Manual execution mode".into(),
        ))
    }
}
