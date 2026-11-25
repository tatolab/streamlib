// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The value source: a node that publishes the value each of its settings is set to on its output
//! of the same name. A node of a project view's type, the plug a Tato Studio view takes in a
//! stream, whose linked inputs a panel reads through a viewer and whose outputs carry what a
//! panel sets their same-named inputs to; a stream's top-level `GraphInput`, whose `value`
//! output carries the value its `value` input is set to; and a `Constant`, which does the same
//! without declaring an Input of its stream.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value as JsonValue;
use streamlib_ipc_types::{ChannelsBlockDescriptorBody, DataChannelPayloadDescriptor};
use streamlib_project_repository::{
    ProjectViewFolderRead, TatolabViewDeclaredPort, TatolabViewPlacement, a_panel_writes,
};

use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::descriptors::{
    DeclaredPinType, NodeClassImportPath, NodeClassShortName, NodeDescriptor, NodeInputMode,
    NodeValueLifetime, PinAnnotations, PinDeclaration, PinFlow, PinType, PortDescriptor,
    SettingReadTime,
};
use crate::core::error::{Error, Result};
use crate::core::execution::{ExecutionConfig, ProcessExecution};
use crate::core::graph::graph_interface_resolution::{
    carry_on_every_value_port, what_the_value_port_of_an_interface_node_carries,
    what_the_value_ports_of_a_constant_carry,
};
use crate::core::graph::input_values::{InputValueRefusal, InputValueRefusalCode};
use crate::core::graph::{GraphNode, GraphNodePorts};
use crate::core::nodes::__generated_private::land_the_unlinked_input_values_in_the_input_mailboxes;
use crate::core::nodes::{DynGeneratedNode, NodeTypesOneStreamResolves};
use crate::core::nodes::{THE_DESCRIPTOR_OF_A_CONSTANT, the_node_type_of_a_graph_input};
pub use crate::core::pin_value_packing::VIEW_NODE_ARRAY_OUTPUT_ATTRIBUTE_NAME;
use crate::core::pin_value_packing::{PackedPinValue, pack_pin_value};
use crate::iceoryx2::{
    InputMailboxes, InputMailboxesInner, MessageGroupBoundary, OutputWriter, OutputWriterInner,
};

mod view_node_settings;

pub(crate) use view_node_settings::render_the_settings_of_each_view_node_into;

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
        .chain(
            manifest
                .outputs
                .iter()
                .filter(|port| a_panel_writes(&port.pin_type))
                .map(the_input_of_an_output_a_panel_writes),
        )
        .collect();
    descriptor.outputs = manifest
        .outputs
        .iter()
        .map(|port| {
            if a_panel_writes(&port.pin_type) {
                the_port_a_view_declares(port).with_pin_declaration(written_as_a_value(port))
            } else {
                the_port_a_view_declares(port)
            }
        })
        .collect();
    Ok(descriptor)
}

/// The input of a view node whose value its output `port`, one a panel writes, publishes: of the
/// output's name and description, a setting read each run that takes no wire and holds no saved
/// value, and returned to no value at Stop, since what a panel writes is live data. It takes a
/// value of the output's pin type where that type holds one; an Audio block or a Tensor, which
/// holds none, it takes as any value, which the output's type packs when it is published.
fn the_input_of_an_output_a_panel_writes(port: &TatolabViewDeclaredPort) -> PortDescriptor {
    let pin_declaration = if port.pin_type.holds_a_stored_value() {
        exactly_as_a_value(&port.pin_type)
    } else {
        carried_as_a_value(Vec::new())
    };
    the_port_a_view_declares(port)
        .with_pin_declaration(pin_declaration)
        .with_input_mode(NodeInputMode::SettingOnly {
            read: SettingReadTime::EachRun,
        })
        .with_lifetime(NodeValueLifetime::Reset)
}

/// Whether input `input` of a node of `node_type` is the input of an output a panel writes, on a
/// view this stream's project brings: a setting of the name of one of the type's outputs.
pub(crate) fn is_the_input_of_an_output_a_panel_writes(
    node_types: &NodeTypesOneStreamResolves,
    node_type: &NodeClassImportPath,
    input: &str,
) -> bool {
    node_types.is_a_view_type_of_this_streams_project(node_type)
        && node_types.port_info(node_type).is_some_and(|ports| {
            is_among_the_ports_of_a_view_the_input_of_an_output_a_panel_writes(&ports, input)
        })
}

/// Whether input `input`, among `ports`, the ports of a view node, is the input of an output a
/// panel writes: a setting of the name of one of the outputs.
fn is_among_the_ports_of_a_view_the_input_of_an_output_a_panel_writes(
    ports: &GraphNodePorts,
    input: &str,
) -> bool {
    ports.outputs.iter().any(|output| output.name == input)
        && ports
            .inputs
            .iter()
            .any(|port| port.name == input && port.is_a_setting())
}

/// An input carried as a value of any type, which the output it is published on packs.
fn carried_as_a_value(former_names: Vec<String>) -> PinDeclaration {
    PinDeclaration {
        pin_type: DeclaredPinType::Any,
        flow: PinFlow::Value,
        annotations: PinAnnotations::default(),
        former_names,
        may_be_unset: false,
    }
}

/// A port of exactly `pin_type`, read as a value; an output so declared keeps its last message
/// for a reader that connects later.
fn exactly_as_a_value(pin_type: &PinType) -> PinDeclaration {
    PinDeclaration {
        flow: PinFlow::Value,
        ..PinDeclaration::exactly(pin_type.clone())
    }
}

/// The pin an output `port`, one a panel writes, declares: exactly its pin type, read as a value,
/// which keeps its last message for a reader that connects later, where that type flows as a
/// value; else, for an Audio block, which flows only as a stream, as a stream.
fn written_as_a_value(port: &TatolabViewDeclaredPort) -> PinDeclaration {
    if port.pin_type.flows().contains(&PinFlow::Value) {
        exactly_as_a_value(&port.pin_type)
    } else {
        PinDeclaration::exactly(port.pin_type.clone())
    }
}

/// Refuse `value`, saved on input `input` of the node named `node_name` of `node_type`, when it is
/// the input of an output a panel writes, which holds what a panel writes, live, and no saved
/// value; taking a value off is no refusal.
pub(crate) fn refuse_a_saved_value_on_the_input_of_an_output_a_panel_writes(
    node_types: &NodeTypesOneStreamResolves,
    node_type: &NodeClassImportPath,
    node_name: &str,
    input: &str,
    value: Option<&JsonValue>,
) -> std::result::Result<(), InputValueRefusal> {
    if value.is_none() || !is_the_input_of_an_output_a_panel_writes(node_types, node_type, input) {
        return Ok(());
    }
    Err(InputValueRefusal {
        code: InputValueRefusalCode::InputTakesNoValue,
        words: format!(
            "input `{input}` of `{node_name}` holds what a panel writes onto its output \
             `{input}`, live, and no saved value: it returns to no value at Stop. To fix: set it \
             with `mode` `live`, in a transaction of value sets alone"
        ),
    })
}

/// `pins`, the saved values given to the node named `node_name` of `node_type`, whose ports are
/// `ports`, without any on the input of an output a panel writes, which holds no saved value;
/// each dropped is logged, naming the node and the input. A node of a type that is no view of
/// this stream's project keeps every one.
pub(crate) fn the_saved_values_without_any_on_an_output_a_panel_writes(
    node_types: &NodeTypesOneStreamResolves,
    node_type: &NodeClassImportPath,
    ports: &GraphNodePorts,
    node_name: &str,
    mut pins: serde_json::Map<String, JsonValue>,
) -> serde_json::Map<String, JsonValue> {
    if !node_types.is_a_view_type_of_this_streams_project(node_type) {
        return pins;
    }
    pins.retain(|input, _| {
        let holds_no_saved_value =
            is_among_the_ports_of_a_view_the_input_of_an_output_a_panel_writes(ports, input);
        if holds_no_saved_value {
            tracing::warn!(
                node = node_name,
                input = input.as_str(),
                "a saved value on an input that holds only what a panel writes live is dropped"
            );
        }
        !holds_no_saved_value
    });
    pins
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

/// The port `port` declares: of exactly its pin type.
fn the_port_a_view_declares(port: &TatolabViewDeclaredPort) -> PortDescriptor {
    PortDescriptor::new(
        port.name.clone(),
        port.description.clone().unwrap_or_default(),
        false,
        PinDeclaration::exactly(port.pin_type.clone()),
    )
}

/// The node that is the source of the values a client sets, a stream's top-level `GraphInput` or a
/// view node: it publishes the value each input of the name of one of its outputs reads on that
/// output, once as it is handed its values when it starts and again each time one changes. Each
/// output is a value, which keeps its last message for a reader that connects later.
pub(crate) struct ValueSourceNode {
    node_name: String,
    descriptor: NodeDescriptor,
    pin_type_of_each_output: HashMap<String, PinType>,
    current_value_of_each_output: HashMap<String, JsonValue>,
    next_start_sample_of_each_channels_output: HashMap<String, u64>,
    output_writer: Option<OutputWriter>,
    input_mailboxes: Option<InputMailboxes>,
}

/// One message a [`ValueSourceNode`] is about to publish: on output `port`, the value `value` as
/// `payload`, and, for a Channels block, the sample the next block of `port` starts at once it
/// is written.
struct MessageToPublish<'value> {
    port: &'value str,
    value: &'value JsonValue,
    payload_descriptor: DataChannelPayloadDescriptor,
    payload: Vec<u8>,
    channels_start_sample_after: Option<u64>,
}

impl ValueSourceNode {
    /// The value source a top-level `GraphInput` runs as: both its `value` ports of the pin type
    /// its `pin_type` setting chooses, or a link resolved.
    pub(crate) fn for_a_top_level_graph_input(node: &GraphNode) -> Result<Self> {
        Self::with_value_ports_carrying(
            node,
            "GraphInput",
            the_node_type_of_a_graph_input()?,
            what_the_value_port_of_an_interface_node_carries,
        )
    }

    /// The value source a `Constant` runs as: both its `value` ports of the pin type its
    /// `pin_type` setting chooses, else a Float.
    pub(crate) fn for_a_constant(node: &GraphNode) -> Result<Self> {
        Self::with_value_ports_carrying(
            node,
            "Constant",
            THE_DESCRIPTOR_OF_A_CONSTANT.clone(),
            |settings| what_the_value_ports_of_a_constant_carry(settings).map(Some),
        )
    }

    /// The value source `node`, of the node type `type_name` whose `descriptor` declares it,
    /// runs as: its `value` ports carrying what `carried` reads off its settings, or refused
    /// naming the node and why.
    fn with_value_ports_carrying(
        node: &GraphNode,
        type_name: &str,
        mut descriptor: NodeDescriptor,
        carried: impl FnOnce(Option<&JsonValue>) -> std::result::Result<Option<PinType>, String>,
    ) -> Result<Self> {
        let settings = JsonValue::Object(node.input_values.clone());
        let chosen = carried(Some(&settings)).map_err(|why| {
            Error::Configuration(format!("`{type_name}` node `{}`: {why}", node.display_name))
        })?;
        carry_on_every_value_port(
            descriptor.inputs.iter_mut().chain(&mut descriptor.outputs),
            chosen.as_ref(),
        );
        Ok(Self::for_the_node(node, descriptor))
    }

    pub(crate) fn for_the_node(node: &GraphNode, descriptor: NodeDescriptor) -> Self {
        let pin_type_of_each_output = descriptor
            .outputs
            .iter()
            .filter_map(|port| match &port.pin_declaration.as_ref()?.pin_type {
                DeclaredPinType::Is { pin_type } => Some((port.name.clone(), pin_type.clone())),
                _ => None,
            })
            .collect();
        Self {
            node_name: node.display_name.clone(),
            descriptor,
            pin_type_of_each_output,
            current_value_of_each_output: HashMap::new(),
            next_start_sample_of_each_channels_output: HashMap::new(),
            output_writer: None,
            input_mailboxes: None,
        }
    }

    /// The message `value`, written onto output `port`, is sent as: packed as its pin type;
    /// refused in the words of the output's type when it carries no such value.
    fn the_message_to_publish<'value>(
        &self,
        port: &'value str,
        value: &'value JsonValue,
    ) -> Result<MessageToPublish<'value>> {
        let Some(pin_type) = self.pin_type_of_each_output.get(port) else {
            return Err(Error::PortError(format!(
                "node `{}` has no output `{port}` of one pin type; its outputs of one are {}",
                self.node_name,
                the_names_listed(self.pin_type_of_each_output.keys())
            )));
        };
        let next_start_sample = self
            .next_start_sample_of_each_channels_output
            .get(port)
            .copied()
            .unwrap_or(0);
        let PackedPinValue {
            payload_descriptor,
            payload,
        } = pack_pin_value(pin_type, value, next_start_sample).map_err(|why| {
            the_refusal_of_a_value_an_output_does_not_carry(&self.node_name, port, pin_type, &why)
        })?;
        let channels_start_sample_after = (crate::core::pin_value_packing::message_kind_of(pin_type)
            == Some(streamlib_ipc_types::DataChannelPayloadDescriptorKind::ChannelsBlock))
        .then(|| {
            let body = ChannelsBlockDescriptorBody::from_descriptor_body(&payload_descriptor.body);
            next_start_sample + u64::from(body.samples_per_channel)
        });
        Ok(MessageToPublish {
            port,
            value,
            payload_descriptor,
            payload,
            channels_start_sample_after,
        })
    }

    /// Publish the value each input of `input_values` names reads on its output of the same
    /// name: every value of an input of `set_live`, which a client has just set, each set being
    /// one message, and the value of any other input where it differs from what its output
    /// carries; an output whose input reads nothing now keeps what it carries. Every message is
    /// made before any is written, so a value one output does not carry publishes none and
    /// advances no Channels output's start sample. Answers the values of the inputs no output is
    /// named after.
    fn publish_each_input_value_on_its_output(
        &mut self,
        input_values: &serde_json::Map<String, JsonValue>,
        set_live: &[String],
    ) -> Result<serde_json::Map<String, JsonValue>> {
        let mut read_by_an_input_alone = serde_json::Map::new();
        let mut messages = Vec::new();
        for (input, value) in input_values {
            if !self.pin_type_of_each_output.contains_key(input) {
                read_by_an_input_alone.insert(input.clone(), value.clone());
                continue;
            }
            if set_live.contains(input)
                || self.current_value_of_each_output.get(input) != Some(value)
            {
                messages.push(self.the_message_to_publish(input, value)?);
            }
        }
        if messages.is_empty() {
            return Ok(read_by_an_input_alone);
        }
        let output_writer = self.output_writer.as_ref().ok_or_else(|| {
            Error::Runtime(format!(
                "node `{}` holds no output writer yet",
                self.node_name
            ))
        })?;
        for message in messages {
            output_writer.write_bytes_described_by(
                message.port,
                &message.payload,
                output_writer.stream_time_now_ns(),
                MessageGroupBoundary::StartsGroup,
                message.payload_descriptor,
            )?;
            if let Some(start_sample) = message.channels_start_sample_after {
                self.next_start_sample_of_each_channels_output
                    .insert(message.port.to_owned(), start_sample);
            }
            self.current_value_of_each_output
                .insert(message.port.to_owned(), message.value.clone());
        }
        Ok(read_by_an_input_alone)
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

pub(crate) fn the_names_listed<'a>(names: impl Iterator<Item = &'a String>) -> String {
    let mut names: Vec<String> = names.map(|name| format!("`{name}`")).collect();
    if names.is_empty() {
        return "none".to_string();
    }
    names.sort();
    names.join(", ")
}

/// The refusal of a value output `port` of the node named `node_name`, of `pin_type`, does not
/// carry, for `why`.
pub(crate) fn the_refusal_of_a_value_an_output_does_not_carry(
    node_name: &str,
    port: &str,
    pin_type: &PinType,
    why: &str,
) -> Error {
    Error::PortError(format!(
        "output `{port}` of node `{node_name}` carries {pin_type}, and {why}"
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
        self.take_every_waiting_input_message()
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

    fn set_unlinked_input_values(
        &mut self,
        input_values: &serde_json::Map<String, JsonValue>,
    ) -> Result<()> {
        self.set_unlinked_input_values_naming_those_set_live(input_values, &[])
    }

    fn set_unlinked_input_values_naming_those_set_live(
        &mut self,
        input_values: &serde_json::Map<String, JsonValue>,
        set_live: &[String],
    ) -> Result<()> {
        let read_by_an_input_alone =
            self.publish_each_input_value_on_its_output(input_values, set_live)?;
        land_the_unlinked_input_values_in_the_input_mailboxes(
            self.iceoryx2_input_mailboxes_inner(),
            &read_by_an_input_alone,
        )
    }

    fn to_runtime_json(&self) -> JsonValue {
        JsonValue::Null
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests;
