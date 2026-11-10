// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A node of a project view's type: the plug a Tato Studio view takes in a stream, whose inputs
//! a panel reads through a viewer and whose outputs carry what a panel writes onto them.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value as JsonValue;
use streamlib_ipc_types::{
    ArrayFrameCpuAttributeToPack, ChannelsBlockDescriptorBody, ChannelsBlockSampleType,
    DataChannelPayloadDescriptor, array_frame_payload_of_cpu_attributes,
};
use streamlib_project_repository::{
    ProjectViewFolderRead, TatolabViewDeclaredPort, TatolabViewPlacement,
};

use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::descriptors::{
    PortDescriptor, PortFamily, PortFamilyDeclaration, ProcessorClassImportPath,
    ProcessorClassShortName, ProcessorDescriptor,
};
use crate::core::error::{Error, Result};
use crate::core::execution::{ExecutionConfig, ProcessExecution};
use crate::core::graph::ProcessorNode;
use crate::core::processors::DynGeneratedProcessor;
use crate::iceoryx2::{
    InputMailboxes, InputMailboxesInner, MessageGroupBoundary, OutputWriter, OutputWriterInner,
};

/// The Array attribute a number or a list of numbers written onto an Array output is carried as.
pub const VIEW_NODE_ARRAY_OUTPUT_ATTRIBUTE_NAME: &str = "values";

/// The element type an Array attribute written onto a view node's output carries.
const VIEW_NODE_ARRAY_OUTPUT_DTYPE: &str = "float64";

/// The sample type a Channels block written onto a view node's output carries.
const VIEW_NODE_CHANNELS_OUTPUT_SAMPLE_TYPE: ChannelsBlockSampleType = ChannelsBlockSampleType::F32;

/// The node type the view `view` is in a stream, or why no node takes it: a view that does not
/// open, or one placed in an area, which has no node.
pub(crate) fn the_node_type_of_a_project_view(
    view: &ProjectViewFolderRead,
) -> std::result::Result<ProcessorDescriptor, String> {
    let manifest = view
        .manifest
        .as_ref()
        .map_err(|why| format!("the view `{}` does not open: {why}", view.view_type))?;
    if manifest.placement != TatolabViewPlacement::Node {
        return Err(format!(
            "the view `{}` is placed in an area, so no node is of its type; a view whose \
             `placement` is `node` is",
            view.view_type
        ));
    }
    let short_name = ProcessorClassShortName::new(manifest.title.clone())
        .map_err(|refusal| format!("the view `{}`: {refusal}", view.view_type))?;
    let node_type = ProcessorClassImportPath::new(view.view_type.clone())
        .map_err(|refusal| format!("the view `{}`: {refusal}", view.view_type))?;
    let mut descriptor = ProcessorDescriptor::new(
        short_name,
        node_type,
        manifest.description.clone().unwrap_or_default(),
    )
    .with_config_schema(
        manifest
            .settings
            .clone()
            .unwrap_or_else(|| serde_json::json!({ "type": "object" })),
    );
    descriptor.inputs = manifest
        .inputs
        .iter()
        .map(the_port_a_view_declares)
        .collect();
    descriptor.outputs = manifest
        .outputs
        .iter()
        .map(the_port_a_view_declares)
        .collect();
    Ok(descriptor)
}

fn the_port_a_view_declares(port: &TatolabViewDeclaredPort) -> PortDescriptor {
    PortDescriptor {
        name: port.name.clone(),
        description: port.description.clone().unwrap_or_default(),
        required: false,
        delivery_profile: None,
        borrow_quota: None,
        audio_window: None,
        family: PortFamilyDeclaration::of_family(port.family),
        pin_default: None,
    }
}

/// The processor a node of a view's type runs as, in this process: it publishes each value a
/// panel writes onto one of its outputs, and takes in what its inputs are sent, which a panel
/// reads through a viewer of the port linked in.
pub(crate) struct ProjectViewNodeProcessor {
    node_name: String,
    descriptor: ProcessorDescriptor,
    config_json: JsonValue,
    family_of_each_output: HashMap<String, PortFamily>,
    next_start_sample_of_each_channels_output: HashMap<String, u64>,
    output_writer: Option<OutputWriter>,
    input_mailboxes: Option<InputMailboxes>,
}

impl ProjectViewNodeProcessor {
    /// The processor of `node`, of the view type `descriptor` describes.
    pub(crate) fn for_the_node(node: &ProcessorNode, descriptor: ProcessorDescriptor) -> Self {
        let family_of_each_output = descriptor
            .outputs
            .iter()
            .filter_map(|port| {
                let families = port.family.families_an_input_takes();
                (families.len() == 1).then(|| (port.name.clone(), families[0]))
            })
            .collect();
        Self {
            node_name: node.display_name.clone(),
            descriptor,
            config_json: node
                .config
                .clone()
                .unwrap_or_else(|| JsonValue::Object(Default::default())),
            family_of_each_output,
            next_start_sample_of_each_channels_output: HashMap::new(),
            output_writer: None,
            input_mailboxes: None,
        }
    }

    /// Publish `value` on output `port`, stamped with stream time now, as its family carries it.
    pub(crate) fn publish_a_value_written_onto_an_output(
        &mut self,
        port: &str,
        value: &JsonValue,
    ) -> Result<()> {
        let Some(&family) = self.family_of_each_output.get(port) else {
            return Err(Error::PortError(format!(
                "view node `{}` has no output `{port}`; its outputs are {}",
                self.node_name,
                the_names_listed(self.family_of_each_output.keys())
            )));
        };
        let next_start_sample = self
            .next_start_sample_of_each_channels_output
            .get(port)
            .copied()
            .unwrap_or(0);
        let (payload_descriptor, payload) =
            the_message_a_value_written_onto_an_output_is(family, value, next_start_sample)
                .map_err(|why| {
                    Error::PortError(format!(
                        "output `{port}` of view node `{}` carries {}, and {why}",
                        self.node_name,
                        family.declaration_name()
                    ))
                })?;
        let output_writer = self.output_writer.as_ref().ok_or_else(|| {
            Error::Runtime(format!(
                "view node `{}` holds no output writer yet",
                self.node_name
            ))
        })?;
        output_writer.write_bytes_described_by(
            port,
            &payload,
            output_writer.stream_time_now_ns(),
            MessageGroupBoundary::StartsGroup,
            payload_descriptor,
        )?;
        if family == PortFamily::Channels {
            let body = ChannelsBlockDescriptorBody::from_descriptor_body(&payload_descriptor.body);
            self.next_start_sample_of_each_channels_output.insert(
                port.to_string(),
                next_start_sample + u64::from(body.samples_per_channel),
            );
        }
        Ok(())
    }

    fn take_every_waiting_input_message(&self) -> Result<()> {
        let Some(input_mailboxes) = self.input_mailboxes.as_ref() else {
            return Ok(());
        };
        for input in &self.descriptor.inputs {
            if !input_mailboxes.has_port(&input.name) {
                continue;
            }
            while input_mailboxes.read_raw(&input.name)?.is_some() {}
        }
        Ok(())
    }
}

fn the_names_listed<'a>(names: impl Iterator<Item = &'a String>) -> String {
    let mut names: Vec<String> = names.map(|name| format!("`{name}`")).collect();
    if names.is_empty() {
        return "none".to_string();
    }
    names.sort();
    names.join(", ")
}

/// The message `value`, written onto an output of `family`, is sent as: a Records output takes
/// a JSON object; an Array output a number or a list of numbers, nested to any depth, as its
/// `values` attribute, or an object of such attributes by name; a Channels output a number, as
/// one control value, or `{"samples": [[...], ...], "sample_rate"}` channel by channel, starting
/// at `next_start_sample`. Image and Packets are not written from a panel.
pub(crate) fn the_message_a_value_written_onto_an_output_is(
    family: PortFamily,
    value: &JsonValue,
    next_start_sample: u64,
) -> std::result::Result<(DataChannelPayloadDescriptor, Vec<u8>), String> {
    match family {
        PortFamily::Records => {
            if !value.is_object() {
                return Err("a Records value is a JSON object".to_string());
            }
            let payload = rmp_serde::to_vec_named(value)
                .map_err(|why| format!("the record does not encode: {why}"))?;
            Ok((DataChannelPayloadDescriptor::records_message(), payload))
        }
        PortFamily::Array => the_array_frame_of_a_value(value),
        PortFamily::Channels => the_channels_block_of_a_value(value, next_start_sample),
        PortFamily::Image | PortFamily::Packets => Err(format!(
            "a panel writes Array, Channels and Records values, never {}",
            family.declaration_name()
        )),
    }
}

/// An Array attribute read off a JSON value: its shape, outermost first, and its elements.
struct ArrayAttributeOfAJsonValue {
    name: String,
    shape: Vec<u64>,
    data: Vec<u8>,
}

fn the_array_frame_of_a_value(
    value: &JsonValue,
) -> std::result::Result<(DataChannelPayloadDescriptor, Vec<u8>), String> {
    let attributes: Vec<ArrayAttributeOfAJsonValue> = match value {
        JsonValue::Object(attributes_by_name) if !attributes_by_name.is_empty() => {
            attributes_by_name
                .iter()
                .map(|(name, attribute)| the_array_attribute_of_a_value(name, attribute))
                .collect::<std::result::Result<_, _>>()?
        }
        JsonValue::Object(_) => return Err("an Array value names at least one attribute".into()),
        _ => vec![the_array_attribute_of_a_value(
            VIEW_NODE_ARRAY_OUTPUT_ATTRIBUTE_NAME,
            value,
        )?],
    };
    let attributes_to_pack: Vec<ArrayFrameCpuAttributeToPack<'_>> = attributes
        .iter()
        .map(|attribute| ArrayFrameCpuAttributeToPack {
            name: &attribute.name,
            dtype: VIEW_NODE_ARRAY_OUTPUT_DTYPE,
            shape: &attribute.shape,
            data: &attribute.data,
        })
        .collect();
    let (body, payload) = array_frame_payload_of_cpu_attributes(&attributes_to_pack)
        .map_err(|why| why.to_string())?;
    Ok((DataChannelPayloadDescriptor::array_frame(body), payload))
}

fn the_array_attribute_of_a_value(
    name: &str,
    value: &JsonValue,
) -> std::result::Result<ArrayAttributeOfAJsonValue, String> {
    let mut shape = Vec::new();
    let mut level = value;
    while let JsonValue::Array(elements) = level {
        shape.push(elements.len() as u64);
        match elements.first() {
            Some(first) => level = first,
            None => break,
        }
    }
    let mut elements = Vec::new();
    the_elements_of_a_value_shaped(value, &shape, &mut elements).map_err(|why| {
        format!("attribute `{name}` is a number or a list of numbers of one shape: {why}")
    })?;
    if shape.is_empty() {
        shape.push(1);
    }
    Ok(ArrayAttributeOfAJsonValue {
        name: name.to_string(),
        shape,
        data: elements
            .iter()
            .flat_map(|element: &f64| element.to_le_bytes())
            .collect(),
    })
}

fn the_elements_of_a_value_shaped(
    value: &JsonValue,
    shape: &[u64],
    elements: &mut Vec<f64>,
) -> std::result::Result<(), String> {
    match (value, shape.split_first()) {
        (JsonValue::Number(number), None) => {
            elements.push(number.as_f64().ok_or("a number is out of range")?);
            Ok(())
        }
        (JsonValue::Array(nested), Some((&length, inner_shape)))
            if nested.len() as u64 == length =>
        {
            nested.iter().try_for_each(|nested| {
                the_elements_of_a_value_shaped(nested, inner_shape, elements)
            })
        }
        (JsonValue::Array(_), Some(_)) => Err("its lists differ in length".into()),
        _ => Err(format!("`{value}` is not a number at that depth")),
    }
}

fn the_channels_block_of_a_value(
    value: &JsonValue,
    start_sample: u64,
) -> std::result::Result<(DataChannelPayloadDescriptor, Vec<u8>), String> {
    let (channels, sample_rate) = match value {
        JsonValue::Number(number) => (
            vec![vec![number.as_f64().ok_or("the number is out of range")?]],
            0,
        ),
        JsonValue::Object(block) => {
            let channels = block
                .get("samples")
                .and_then(JsonValue::as_array)
                .ok_or("a Channels block is `{\"samples\": [[...], ...], \"sample_rate\"}`")?
                .iter()
                .map(|channel| {
                    channel
                        .as_array()
                        .ok_or("each channel of `samples` is a list of numbers")?
                        .iter()
                        .map(|sample| sample.as_f64().ok_or("each sample is a number"))
                        .collect::<std::result::Result<Vec<f64>, _>>()
                })
                .collect::<std::result::Result<Vec<Vec<f64>>, _>>()?;
            let sample_rate = match block.get("sample_rate") {
                None => 0,
                Some(rate) => rate
                    .as_u64()
                    .and_then(|rate| u32::try_from(rate).ok())
                    .ok_or("`sample_rate` is a whole number of samples per second")?,
            };
            if let Some(key) = block
                .keys()
                .find(|key| !matches!(key.as_str(), "samples" | "sample_rate"))
            {
                return Err(format!("a Channels block takes no `{key}`"));
            }
            (channels, sample_rate)
        }
        _ => {
            return Err(
                "a Channels value is a number or `{\"samples\": [[...], ...], \"sample_rate\"}`"
                    .to_string(),
            );
        }
    };
    let samples_per_channel = channels.first().map_or(0, Vec::len);
    if samples_per_channel == 0
        || channels
            .iter()
            .any(|channel| channel.len() != samples_per_channel)
    {
        return Err("every channel holds the same number of samples, at least one".into());
    }
    let channel_count = u32::try_from(channels.len()).map_err(|_| "too many channels")?;
    let samples_per_channel =
        u32::try_from(samples_per_channel).map_err(|_| "too many samples per channel")?;
    let payload = channels
        .iter()
        .flatten()
        .flat_map(|sample| (*sample as f32).to_le_bytes())
        .collect();
    Ok((
        DataChannelPayloadDescriptor::channels_block(ChannelsBlockDescriptorBody {
            start_sample,
            sample_rate,
            channel_count,
            samples_per_channel,
            sample_type: VIEW_NODE_CHANNELS_OUTPUT_SAMPLE_TYPE as u32,
        }),
        payload,
    ))
}

impl DynGeneratedProcessor for ProjectViewNodeProcessor {
    fn __generated_setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn __generated_teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn __generated_on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn __generated_on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        self.take_every_waiting_input_message()
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Err(Error::Runtime(
            "a view node is reactive, so it is never started by hand".into(),
        ))
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Err(Error::Runtime(
            "a view node is reactive, so it is never stopped by hand".into(),
        ))
    }

    fn name(&self) -> &str {
        &self.node_name
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        Some(self.descriptor.clone())
    }

    fn execution_config(&self) -> ExecutionConfig {
        ExecutionConfig::new(ProcessExecution::Reactive)
    }

    fn has_iceoryx2_outputs(&self) -> bool {
        true
    }

    fn has_iceoryx2_inputs(&self) -> bool {
        true
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

    fn apply_config_json(&mut self, config_json: &JsonValue) -> Result<()> {
        self.config_json = config_json.clone();
        Ok(())
    }

    fn to_runtime_json(&self) -> JsonValue {
        JsonValue::Null
    }

    fn config_json(&self) -> JsonValue {
        self.config_json.clone()
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
#[path = "project_view_node_tests.rs"]
mod project_view_node_tests;
