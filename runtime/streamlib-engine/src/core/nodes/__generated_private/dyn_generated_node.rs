// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use super::GeneratedNode;
use super::generated_node::land_the_unlinked_input_values_in_the_input_mailboxes;
use super::generated_node_impl::{
    FunctionNodePromotionAwaitedBeforeTheReaderRestarts, OutOfProcessLinkWiringEnvelope,
};
use crate::core::NodeDescriptor;
use crate::core::Result;
use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::execution::ExecutionConfig;

pub trait DynGeneratedNode: Send + 'static {
    /// Generated setup hook called by runtime with privileged ctx.
    fn __generated_setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()>;

    /// Generated teardown hook called by runtime with privileged ctx.
    fn __generated_teardown(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()>;

    /// Generated on_pause hook — restricted ctx.
    fn __generated_on_pause(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()>;

    /// Generated on_resume hook — restricted ctx.
    fn __generated_on_resume(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()>;

    fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()>;

    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()>;

    fn stop(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()>;

    fn name(&self) -> &str;
    fn descriptor(&self) -> Option<NodeDescriptor>;

    fn execution_config(&self) -> ExecutionConfig;

    fn has_iceoryx2_outputs(&self) -> bool;

    fn has_iceoryx2_inputs(&self) -> bool;

    /// Install host-allocated iceoryx2 resources (issue #894).
    fn set_iceoryx2_resources(
        &mut self,
        output_writer: Option<crate::iceoryx2::OutputWriter>,
        input_mailboxes: Option<crate::iceoryx2::InputMailboxes>,
    ) -> crate::core::Result<()>;

    /// Borrow the host-side `OutputWriterInner` Arc.
    fn iceoryx2_output_writer_inner(
        &self,
    ) -> Option<std::sync::Arc<crate::iceoryx2::OutputWriterInner>>;

    /// Borrow the host-side `InputMailboxesInner` Arc.
    fn iceoryx2_input_mailboxes_inner(
        &self,
    ) -> Option<std::sync::Arc<crate::iceoryx2::InputMailboxesInner>>;

    fn detect_and_clean_up_after_an_out_of_process_helper_that_died(&mut self) {}

    fn has_failed_unrecoverably(&self) -> bool {
        false
    }

    fn failure_the_node_reported_of_itself(&self) -> Option<String> {
        None
    }

    fn why_process_is_raising_as_its_helper_reports(&self) -> Option<String> {
        None
    }

    fn why_the_node_is_not_ready_as_its_helper_reports(&self) -> Option<String> {
        None
    }

    fn why_the_out_of_process_helper_died_on_its_own(&self) -> Option<String> {
        None
    }

    fn ask_to_promote_the_function_node_that_lost_the_helper_that_died(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> Option<FunctionNodePromotionAwaitedBeforeTheReaderRestarts> {
        None
    }

    fn restart_the_out_of_process_helper_that_died(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> crate::core::Result<()> {
        Err(crate::core::Error::Runtime(
            "this node runs in the app process, so it has no helper process to restart".to_string(),
        ))
    }

    fn out_of_process_link_wiring(&self) -> Option<Arc<OutOfProcessLinkWiringEnvelope>> {
        None
    }

    /// Take `input_values`, the value each input reads while no link feeds it, by input, in
    /// place of those it took before; by default each lands in its input mailboxes, which answer
    /// a read of the input with it while no link feeds it.
    fn set_unlinked_input_values(
        &mut self,
        input_values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<()> {
        land_the_unlinked_input_values_in_the_input_mailboxes(
            self.iceoryx2_input_mailboxes_inner(),
            input_values,
        )
    }

    /// Take `input_values` as [`Self::set_unlinked_input_values`] does, where each input of
    /// `set_live` was just set live by a client; by default a node takes them alike.
    fn set_unlinked_input_values_naming_those_set_live(
        &mut self,
        input_values: &serde_json::Map<String, serde_json::Value>,
        _set_live: &[String],
    ) -> Result<()> {
        self.set_unlinked_input_values(input_values)
    }

    fn to_runtime_json(&self) -> serde_json::Value;

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

impl<T> DynGeneratedNode for T
where
    T: GeneratedNode,
{
    fn __generated_setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        <Self as GeneratedNode>::__generated_setup(self, ctx)
    }

    fn __generated_teardown(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        <Self as GeneratedNode>::__generated_teardown(self, ctx)
    }

    fn __generated_on_pause(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        <Self as GeneratedNode>::__generated_on_pause(self, ctx)
    }

    fn __generated_on_resume(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        <Self as GeneratedNode>::__generated_on_resume(self, ctx)
    }

    fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        <Self as GeneratedNode>::process(self, ctx)
    }

    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        <Self as GeneratedNode>::start(self, ctx)
    }

    fn stop(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        <Self as GeneratedNode>::stop(self, ctx)
    }

    fn name(&self) -> &str {
        <Self as GeneratedNode>::name(self)
    }

    fn descriptor(&self) -> Option<NodeDescriptor> {
        <T as GeneratedNode>::descriptor()
    }

    fn execution_config(&self) -> ExecutionConfig {
        <Self as GeneratedNode>::execution_config(self)
    }

    fn has_iceoryx2_outputs(&self) -> bool {
        <Self as GeneratedNode>::has_iceoryx2_outputs(self)
    }

    fn has_iceoryx2_inputs(&self) -> bool {
        <Self as GeneratedNode>::has_iceoryx2_inputs(self)
    }

    fn set_iceoryx2_resources(
        &mut self,
        output_writer: Option<crate::iceoryx2::OutputWriter>,
        input_mailboxes: Option<crate::iceoryx2::InputMailboxes>,
    ) -> crate::core::Result<()> {
        <Self as GeneratedNode>::set_iceoryx2_resources(self, output_writer, input_mailboxes)
    }

    fn iceoryx2_output_writer_inner(
        &self,
    ) -> Option<std::sync::Arc<crate::iceoryx2::OutputWriterInner>> {
        <Self as GeneratedNode>::iceoryx2_output_writer_inner(self)
    }

    fn iceoryx2_input_mailboxes_inner(
        &self,
    ) -> Option<std::sync::Arc<crate::iceoryx2::InputMailboxesInner>> {
        <Self as GeneratedNode>::iceoryx2_input_mailboxes_inner(self)
    }

    fn set_unlinked_input_values(
        &mut self,
        input_values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<()> {
        <Self as GeneratedNode>::set_unlinked_input_values(self, input_values)
    }

    fn to_runtime_json(&self) -> serde_json::Value {
        <Self as GeneratedNode>::to_runtime_json(self)
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
