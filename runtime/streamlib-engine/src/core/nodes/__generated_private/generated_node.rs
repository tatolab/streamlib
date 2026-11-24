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

    /// Take `input_values`, the value each input reads while no link feeds it, by input, in
    /// place of those it took before; by default the node takes the settings they hold, then
    /// each lands in its input mailboxes, which answer a read of the input with it while no link
    /// feeds it.
    fn set_unlinked_input_values(
        &mut self,
        input_values: &serde_json::Map<String, JsonValue>,
    ) -> Result<()> {
        self.take_the_settings_the_input_values_hold(input_values)?;
        land_the_unlinked_input_values_in_the_input_mailboxes(
            self.iceoryx2_input_mailboxes_inner(),
            input_values,
        )
    }

    /// Take the settings `input_values` hold, each read at setup, until the node is set up;
    /// refused when one holds a value its setting does not take. A node type declaring no
    /// settings takes none.
    fn take_the_settings_the_input_values_hold(
        &mut self,
        _input_values: &serde_json::Map<String, JsonValue>,
    ) -> Result<()> {
        Ok(())
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

/// Land each of `input_values` in `input_mailboxes`, a node's input mailboxes when it has them,
/// which answer a read of an input with its value while no link feeds it: how a node takes the
/// values its unlinked inputs read unless its type takes them itself.
pub(crate) fn land_the_unlinked_input_values_in_the_input_mailboxes(
    input_mailboxes: Option<std::sync::Arc<crate::iceoryx2::InputMailboxesInner>>,
    input_values: &serde_json::Map<String, JsonValue>,
) -> Result<()> {
    if let Some(input_mailboxes) = input_mailboxes {
        input_mailboxes.read_each_unlinked_input_as_its_value(input_values);
    }
    Ok(())
}
