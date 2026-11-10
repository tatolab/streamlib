// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use super::studio_view_node_type::{StudioViewNodeConfig, the_descriptor_of_a_studio_view_node};
use super::studio_view_output_payload::the_payload_of_a_value_written_onto;
use crate::core::ProcessorDescriptor;
use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess, StreamClock};
use crate::core::descriptors::PortFamily;
use crate::core::error::{Error, Result};
use crate::core::execution::{ExecutionConfig, ProcessExecution};
use crate::core::graph::{ProcessorNode, port_names_listed_for_a_refusal};
use crate::core::media_clock::MediaClock;
use crate::core::processors::DynGeneratedProcessor;
use crate::iceoryx2::{
    InputMailboxes, InputMailboxesInner, MessageGroupBoundary, OutputWriter, OutputWriterInner,
};

/// The processor of one StudioView node: it runs nothing of its own, and publishes on an output
/// each value a panel writes onto it.
pub(crate) struct StudioViewProcessor {
    node_name: String,
    config: StudioViewNodeConfig,
    descriptor: ProcessorDescriptor,
    family_of_each_output: HashMap<String, PortFamily>,
    next_start_sample_of_each_channels_output: HashMap<String, u64>,
    stream_clock: Option<Arc<StreamClock>>,
    output_writer: Option<OutputWriter>,
    input_mailboxes: Option<InputMailboxes>,
}

impl StudioViewProcessor {
    /// The processor of the StudioView node `node`, its ports the ones the node took.
    pub(crate) fn of_the_node(node: &ProcessorNode) -> Result<Self> {
        let config = StudioViewNodeConfig::of_the_node(
            &node.display_name,
            node.config.as_ref().unwrap_or(&Value::Null),
        )?;
        Ok(Self {
            node_name: node.display_name.clone(),
            config,
            descriptor: the_descriptor_of_a_studio_view_node(node),
            family_of_each_output: node
                .ports
                .outputs
                .iter()
                .map(|output| (output.name.clone(), output.family.family))
                .collect(),
            next_start_sample_of_each_channels_output: HashMap::new(),
            stream_clock: None,
            output_writer: None,
            input_mailboxes: None,
        })
    }

    /// Publish `value` on output `port`, as the payload of the port's family.
    pub(crate) fn publish_a_value_written_onto_an_output(
        &mut self,
        port: &str,
        value: &Value,
    ) -> Result<()> {
        let Some(&family) = self.family_of_each_output.get(port) else {
            return Err(Error::Configuration(format!(
                "StudioView node `{}` (view `{}`) has no output `{port}`. Its outputs are: {}",
                self.node_name,
                self.config.view_type,
                port_names_listed_for_a_refusal(
                    self.descriptor
                        .outputs
                        .iter()
                        .map(|output| output.name.as_str())
                )
            )));
        };
        let start_sample = self
            .next_start_sample_of_each_channels_output
            .get(port)
            .copied()
            .unwrap_or_default();
        let written = the_payload_of_a_value_written_onto(port, family, value, start_sample)
            .map_err(|refusal| {
                Error::Configuration(format!(
                    "StudioView node `{}` takes no such value: {refusal}",
                    self.node_name
                ))
            })?;
        let timestamp_ns = match &self.stream_clock {
            Some(stream_clock) => stream_clock.now_ns(),
            None => MediaClock::now().as_nanos() as i64,
        };
        let output_writer = self.output_writer.as_ref().ok_or_else(|| {
            Error::Runtime(format!(
                "StudioView node `{}` holds no output writer to publish on",
                self.node_name
            ))
        })?;
        output_writer.write_bytes_described_by(
            port,
            &written.payload,
            timestamp_ns,
            MessageGroupBoundary::StartsGroup,
            written.payload_descriptor,
        )?;
        if family == PortFamily::Channels {
            self.next_start_sample_of_each_channels_output
                .insert(port.to_string(), start_sample + written.samples_per_channel);
        }
        Ok(())
    }
}

impl DynGeneratedProcessor for StudioViewProcessor {
    fn __generated_setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.stream_clock = Some(Arc::clone(&ctx.host_base().stream_clock));
        Ok(())
    }

    fn __generated_teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.stream_clock = None;
        Ok(())
    }

    fn __generated_on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn __generated_on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Err(Error::Runtime(
            "a StudioView node is driven by the values written onto it, so it has no process \
             step"
                .into(),
        ))
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn name(&self) -> &str {
        &self.node_name
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        Some(self.descriptor.clone())
    }

    fn execution_config(&self) -> ExecutionConfig {
        ExecutionConfig::new(ProcessExecution::Manual)
    }

    fn has_iceoryx2_outputs(&self) -> bool {
        !self.descriptor.outputs.is_empty()
    }

    fn has_iceoryx2_inputs(&self) -> bool {
        !self.descriptor.inputs.is_empty()
    }

    fn set_iceoryx2_resources(
        &mut self,
        output_writer: Option<OutputWriter>,
        input_mailboxes: Option<InputMailboxes>,
    ) -> Result<()> {
        if output_writer.is_some() {
            self.output_writer = output_writer;
        }
        if input_mailboxes.is_some() {
            self.input_mailboxes = input_mailboxes;
        }
        Ok(())
    }

    fn iceoryx2_output_writer_inner(&self) -> Option<Arc<OutputWriterInner>> {
        self.output_writer
            .as_ref()
            .and_then(OutputWriter::inner_arc)
    }

    fn iceoryx2_input_mailboxes_inner(&self) -> Option<Arc<InputMailboxesInner>> {
        self.input_mailboxes
            .as_ref()
            .and_then(InputMailboxes::inner_arc)
    }

    fn apply_config_json(&mut self, config_json: &Value) -> Result<()> {
        let config = StudioViewNodeConfig::of_the_node(&self.node_name, config_json)?;
        if config.view_type != self.config.view_type {
            return Err(Error::Configuration(format!(
                "StudioView node `{}` shows the view `{}`, and its ports are that view's; a node \
                 showing `{}` is another node",
                self.node_name, self.config.view_type, config.view_type
            )));
        }
        self.config = config;
        Ok(())
    }

    fn to_runtime_json(&self) -> Value {
        Value::Null
    }

    fn config_json(&self) -> Value {
        serde_json::to_value(&self.config).unwrap_or(Value::Null)
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
