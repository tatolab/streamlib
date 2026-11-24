// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node of a type a test registers taking a config of its own: the node `P` builds, which
//! takes a config edit when `C` would take it.

use std::marker::PhantomData;
use std::sync::Arc;

use serde_json::Value as JsonValue;

use crate::core::NodeDescriptor;
use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::error::{Error, Result};
use crate::core::execution::ExecutionConfig;
use crate::core::nodes::{Config, GeneratedNode};

/// The node `P` builds, taking an edit of its config when it is a config `C` would take, and
/// handing `P` nothing of it, since a Rust node holds no config.
pub(super) struct NodeTakingAConfigForATest<P, C> {
    node: P,
    takes_a_config_of: PhantomData<fn() -> C>,
}

impl<P, C> GeneratedNode for NodeTakingAConfigForATest<P, C>
where
    P: GeneratedNode,
    C: Config,
{
    type Config = P::Config;

    fn name(&self) -> &str {
        self.node.name()
    }

    fn from_config(config: Self::Config) -> Result<Self> {
        Ok(Self {
            node: P::from_config(config)?,
            takes_a_config_of: PhantomData,
        })
    }

    fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        self.node.process(ctx)
    }

    fn apply_config_json(&mut self, config_json: &JsonValue) -> Result<()> {
        serde_json::from_value::<C>(config_json.clone())
            .map(drop)
            .map_err(|refusal| Error::Config(refusal.to_string()))
    }

    fn set_unlinked_input_values(
        &mut self,
        input_values: &serde_json::Map<String, JsonValue>,
    ) -> Result<()> {
        self.node.set_unlinked_input_values(input_values)
    }

    fn execution_config(&self) -> ExecutionConfig {
        self.node.execution_config()
    }

    fn descriptor() -> Option<NodeDescriptor> {
        P::descriptor()
    }

    fn has_iceoryx2_outputs(&self) -> bool {
        self.node.has_iceoryx2_outputs()
    }

    fn has_iceoryx2_inputs(&self) -> bool {
        self.node.has_iceoryx2_inputs()
    }

    fn set_iceoryx2_resources(
        &mut self,
        output_writer: Option<crate::iceoryx2::OutputWriter>,
        input_mailboxes: Option<crate::iceoryx2::InputMailboxes>,
    ) -> Result<()> {
        self.node
            .set_iceoryx2_resources(output_writer, input_mailboxes)
    }

    fn iceoryx2_output_writer_inner(&self) -> Option<Arc<crate::iceoryx2::OutputWriterInner>> {
        self.node.iceoryx2_output_writer_inner()
    }

    fn iceoryx2_input_mailboxes_inner(&self) -> Option<Arc<crate::iceoryx2::InputMailboxesInner>> {
        self.node.iceoryx2_input_mailboxes_inner()
    }

    fn to_runtime_json(&self) -> JsonValue {
        self.node.to_runtime_json()
    }

    fn config_json(&self) -> JsonValue {
        self.node.config_json()
    }

    fn __generated_setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.node.__generated_setup(ctx)
    }

    fn __generated_teardown(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.node.__generated_teardown(ctx)
    }

    fn __generated_on_pause(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        self.node.__generated_on_pause(ctx)
    }

    fn __generated_on_resume(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        self.node.__generated_on_resume(ctx)
    }

    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.node.start(ctx)
    }

    fn stop(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.node.stop(ctx)
    }
}
