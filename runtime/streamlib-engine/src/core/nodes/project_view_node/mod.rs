// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The value source: a node of a project view's type, the plug a Tato Studio view takes in a
//! stream, whose inputs a panel reads through a viewer and whose outputs carry what a panel
//! writes onto them; and a stream's top-level `GraphInput`, whose `value` output carries the
//! value its `value` input is set to.

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
    DeclaredPinType, NodeClassImportPath, NodeClassShortName, NodeDescriptor, NodeInputMode,
    PinAnnotations, PinDeclaration, PinFlow, PortDescriptor, PortFamily, PortFamilyDeclaration,
    SettingReadTime,
};
use crate::core::error::{Error, Result};
use crate::core::execution::{ExecutionConfig, ProcessExecution};
use crate::core::graph::GraphNode;
use crate::core::graph::graph_interface_resolution::{
    GRAPH_INPUT_NODE_TYPE, GRAPH_INPUT_VALUE_FORMER_NAME, GRAPH_INTERFACE_FAMILY_CONFIG_KEY,
    GRAPH_INTERFACE_VALUE_PORT_NAME,
};
use crate::core::nodes::__generated_private::land_the_unlinked_input_values_in_the_input_mailboxes;
use crate::core::nodes::DynGeneratedNode;
use crate::iceoryx2::{
    InputMailboxes, InputMailboxesInner, MessageGroupBoundary, OutputWriter, OutputWriterInner,
};

/// The Array attribute a number or a list of numbers written onto an Array output is carried as.
pub const VIEW_NODE_ARRAY_OUTPUT_ATTRIBUTE_NAME: &str = "values";

/// The element type an Array attribute written onto a view node's output carries.
const VIEW_NODE_ARRAY_OUTPUT_DTYPE: &str = "float64";

/// The sample type a Channels block written onto a view node's output carries.
const VIEW_NODE_CHANNELS_OUTPUT_SAMPLE_TYPE: ChannelsBlockSampleType = ChannelsBlockSampleType::F32;

/// How a link into a view node's input delivers: the node itself reads nothing, a panel reads
/// the port linked into it through a viewer, so the link never holds its writer back.
const VIEW_NODE_INPUT_DELIVERY_PROFILE: &str = "newest";

/// The node type the view `view` is in a stream, or why no node takes it: a view that does not
/// open, or one placed in an area, which has no node.
pub(crate) fn the_node_type_of_a_project_view(
    view: &ProjectViewFolderRead,
) -> std::result::Result<NodeDescriptor, String> {
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
    let short_name = NodeClassShortName::new(manifest.title.clone())
        .map_err(|refusal| format!("the view `{}`: {refusal}", view.view_type))?;
    let node_type = NodeClassImportPath::new(view.view_type.clone())
        .map_err(|refusal| format!("the view `{}`: {refusal}", view.view_type))?;
    let mut descriptor = NodeDescriptor::new(
        short_name,
        node_type,
        manifest.description.clone().unwrap_or_default(),
    )
    .with_config_schema(the_config_schema_of_a_view_node(manifest.settings.as_ref()));
    descriptor.inputs = manifest
        .inputs
        .iter()
        .map(|port| {
            the_port_a_view_declares(port).with_delivery_profile(VIEW_NODE_INPUT_DELIVERY_PROFILE)
        })
        .collect();
    descriptor.outputs = manifest
        .outputs
        .iter()
        .map(the_port_a_view_declares)
        .collect();
    Ok(descriptor)
}

/// The config schema a node of a view takes: the view's `settings`, closed to the settings it
/// declares unless `additionalProperties` says otherwise; a view declaring none takes none.
fn the_config_schema_of_a_view_node(settings: Option<&JsonValue>) -> JsonValue {
    let mut config_schema = settings
        .cloned()
        .unwrap_or_else(|| serde_json::json!({ "type": "object", "properties": {} }));
    if let Some(declaration) = config_schema.as_object_mut() {
        declaration
            .entry("additionalProperties")
            .or_insert(JsonValue::Bool(false));
    }
    config_schema
}

fn the_port_a_view_declares(port: &TatolabViewDeclaredPort) -> PortDescriptor {
    PortDescriptor::new(
        port.name.clone(),
        port.description.clone().unwrap_or_default(),
        false,
        PortFamilyDeclaration::of_family(port.family),
    )
}

/// The descriptor of the node type every top-level `GraphInput` runs as: a `value` input, a
/// setting read each run that holds what a client sets, and a `value` output carrying it as a
/// value, which keeps its last message for a reader that connects later; both of the family its
/// config names.
pub(crate) fn the_node_type_of_a_top_level_graph_input() -> Result<NodeDescriptor> {
    let mut value_family = PortFamilyDeclaration::of_family(PortFamily::Records);
    value_family.family_from_config_key = Some(GRAPH_INTERFACE_FAMILY_CONFIG_KEY.to_string());
    let carried_as_a_value = |former_names: Vec<String>| PinDeclaration {
        pin_type: DeclaredPinType::Any,
        flow: PinFlow::Value,
        annotations: PinAnnotations::default(),
        former_names,
        may_be_unset: false,
    };
    let mut descriptor = NodeDescriptor::new(
        NodeClassShortName::new("GraphInput".to_string())
            .map_err(|refusal| Error::Configuration(refusal.to_string()))?,
        NodeClassImportPath::new(GRAPH_INPUT_NODE_TYPE.to_string())
            .map_err(|refusal| Error::Configuration(refusal.to_string()))?,
        "One Input of a stream: every port `value` is linked to reads what a client sets."
            .to_string(),
    );
    descriptor.inputs = vec![
        PortDescriptor::new(
            GRAPH_INTERFACE_VALUE_PORT_NAME,
            "What a client sets on the Input, of the declared family; it takes no wire.",
            false,
            value_family.clone(),
        )
        .with_pin_declaration(carried_as_a_value(vec![
            GRAPH_INPUT_VALUE_FORMER_NAME.to_string(),
        ]))
        .with_input_mode(NodeInputMode::SettingOnly {
            read: SettingReadTime::EachRun,
        }),
    ];
    descriptor.outputs = vec![
        PortDescriptor::new(
            GRAPH_INTERFACE_VALUE_PORT_NAME,
            "What a client sets on the Input, of the declared family.",
            false,
            value_family,
        )
        .with_pin_declaration(carried_as_a_value(Vec::new())),
    ];
    Ok(descriptor)
}

/// The family declaration of a `GraphInput`'s `value` output in `descriptor`.
fn the_value_family_declared(descriptor: &NodeDescriptor) -> Result<PortFamilyDeclaration> {
    match descriptor.outputs.as_slice() {
        [value] => Ok(value.family.clone()),
        _ => Err(Error::Configuration(
            "a `GraphInput` declares one output, `value`".to_string(),
        )),
    }
}

/// The node that is the source of the values a client sets: a stream's top-level `GraphInput`,
/// which publishes the value its `value` input reads on its `value` output, once as it is handed
/// its values when it starts and again each time it changes, and a view node, onto whose outputs
/// a panel writes and which sends a reader that connects later the value each output carries.
pub(crate) struct ValueSourceNode {
    node_name: String,
    descriptor: NodeDescriptor,
    config_json: JsonValue,
    family_of_each_output: HashMap<String, PortFamily>,
    current_value_of_each_output: HashMap<String, JsonValue>,
    next_start_sample_of_each_channels_output: HashMap<String, u64>,
    output_writer: Option<OutputWriter>,
    input_mailboxes: Option<InputMailboxes>,
    kind: ValueSourceKind,
}

/// Which node a [`ValueSourceNode`] is, which decides where the values a client sets go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueSourceKind {
    /// A stream's top-level `GraphInput`: each input value is published on its same-named
    /// output, which keeps its last message for a reader that connects later.
    TopLevelGraphInput,
    /// A view node: a panel writes onto its outputs, and each reader that connects later is
    /// handed the value each output carries.
    ViewNode,
}

impl ValueSourceNode {
    pub(crate) fn for_a_top_level_graph_input(node: &GraphNode) -> Result<Self> {
        let mut descriptor = the_node_type_of_a_top_level_graph_input()?;
        let family = streamlib_node_schema::output_port_family_resolved_against_config(
            &the_value_family_declared(&descriptor)?,
            node.config.as_ref(),
        )
        .map_err(|why| {
            Error::Configuration(format!("`GraphInput` node `{}`: {why}", node.display_name))
        })?;
        for port in descriptor.inputs.iter_mut().chain(&mut descriptor.outputs) {
            port.family = PortFamilyDeclaration::of_family(family);
        }
        Ok(Self {
            kind: ValueSourceKind::TopLevelGraphInput,
            ..Self::for_the_node(node, descriptor)
        })
    }

    pub(crate) fn for_the_node(node: &GraphNode, descriptor: NodeDescriptor) -> Self {
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
            current_value_of_each_output: HashMap::new(),
            next_start_sample_of_each_channels_output: HashMap::new(),
            output_writer: None,
            input_mailboxes: None,
            kind: ValueSourceKind::ViewNode,
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
                "node `{}` has no output `{port}`; its outputs are {}",
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
                        "output `{port}` of node `{}` carries {}, and {why}",
                        self.node_name,
                        family.declaration_name()
                    ))
                })?;
        let output_writer = self.output_writer.as_ref().ok_or_else(|| {
            Error::Runtime(format!(
                "node `{}` holds no output writer yet",
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
        self.current_value_of_each_output
            .insert(port.to_string(), value.clone());
        Ok(())
    }

    /// Publish the value each input of `input_values` names reads on its output of the same
    /// name, where it differs from what that output carries; an output whose input reads nothing
    /// now keeps what it carries. Answers the values of the inputs no output is named after.
    fn publish_each_input_value_on_its_output(
        &mut self,
        input_values: &serde_json::Map<String, JsonValue>,
    ) -> Result<serde_json::Map<String, JsonValue>> {
        let mut read_by_an_input_alone = serde_json::Map::new();
        for (input, value) in input_values {
            if !self.family_of_each_output.contains_key(input) {
                read_by_an_input_alone.insert(input.clone(), value.clone());
                continue;
            }
            if self.current_value_of_each_output.get(input) != Some(value) {
                self.publish_a_value_written_onto_an_output(input, value)?;
            }
        }
        Ok(read_by_an_input_alone)
    }

    /// Publish the value each output carries again where a reader now connected missed it.
    fn hand_each_current_value_to_a_reader_it_has_not_reached(&mut self) -> Result<()> {
        let Some(output_writer_inner) = self
            .output_writer
            .as_ref()
            .and_then(OutputWriter::inner_arc)
        else {
            return Ok(());
        };
        let outputs_a_reader_missed: Vec<(String, JsonValue)> = self
            .current_value_of_each_output
            .iter()
            .filter(|(port, _)| output_writer_inner.has_a_reader_the_last_write_missed(port))
            .map(|(port, value)| (port.clone(), value.clone()))
            .collect();
        for (port, value) in outputs_a_reader_missed {
            self.publish_a_value_written_onto_an_output(&port, &value)?;
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
/// `values` attribute, or an object of such attributes by name; a Channels output a number or a
/// bool, as one control value, or `{"samples": [[...], ...], "sample_rate"}` channel by channel, starting
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
            "a client writes Array, Channels and Records values, never {}",
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
        JsonValue::Bool(control_value) => (vec![vec![f64::from(u8::from(*control_value))]], 0),
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
                "a Channels value is a number, a bool or `{\"samples\": [[...], ...], \"sample_rate\"}`"
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

impl DynGeneratedNode for ValueSourceNode {
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
        self.take_every_waiting_input_message()?;
        match self.kind {
            ValueSourceKind::TopLevelGraphInput => Ok(()),
            ValueSourceKind::ViewNode => {
                self.hand_each_current_value_to_a_reader_it_has_not_reached()
            }
        }
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Err(Error::Runtime(format!(
            "node `{}` runs on its own schedule, so it is never started by hand",
            self.node_name
        )))
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Err(Error::Runtime(format!(
            "node `{}` runs on its own schedule, so it is never stopped by hand",
            self.node_name
        )))
    }

    fn name(&self) -> &str {
        &self.node_name
    }

    fn descriptor(&self) -> Option<NodeDescriptor> {
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

    fn set_unlinked_input_values(
        &mut self,
        input_values: &serde_json::Map<String, JsonValue>,
    ) -> Result<()> {
        let read_by_an_input_alone = match self.kind {
            ValueSourceKind::TopLevelGraphInput => {
                std::borrow::Cow::Owned(self.publish_each_input_value_on_its_output(input_values)?)
            }
            ValueSourceKind::ViewNode => std::borrow::Cow::Borrowed(input_values),
        };
        land_the_unlinked_input_values_in_the_input_mailboxes(
            self.iceoryx2_input_mailboxes_inner(),
            &read_by_an_input_alone,
        )
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
mod tests;
