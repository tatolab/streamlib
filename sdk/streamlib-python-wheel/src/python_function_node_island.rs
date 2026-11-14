// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A function node island evaluated inside this helper's reads: each node
//! called in turn, once per stream time, through its compiled evaluate entry
//! or as Python in this helper's interpreter.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyInt, PyList};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::{
    BREADCRUMB_NUMBER_OF_THE_FUNCTION_NODE_A_HELPER_RUNS_AS_ITS_PROCESSOR,
    ChannelsBlockDescriptorBody, ChannelsBlockSampleType, DataChannelPayloadDescriptor,
    DataChannelPayloadDescriptorKind, FunctionNodeIslandEvaluatedAtEachRead,
    FunctionNodeIslandOutputForOneRead, FunctionNodeIslandOutputValue,
    GpuPoolSlotFrameDescriptorBody, HelperProcessLossCountBoardWriter, ImageFrameDescriptorBody,
    InputMailboxesInner, MessageAFunctionNodeIslandHandsOnUnread, MessageReadFromInputPort,
};
use tatolab_module_sdk::module_host_table_abi::{
    MODULE_HOST_TABLE_STATUS_OK, ModuleHostTableFunctionNodeEvaluateEntry,
};

use crate::python_bag_conversion::{records_message_dict_of, records_message_payload_of};
use crate::python_helper_stream_clock::helper_stream_clock_now_ns;
use crate::python_logging::monotonic_clock_now_ns;
use crate::python_processor_link_data_access::PythonProcessorLinkDataAccess;

/// One value a function node takes or gives.
#[derive(Debug)]
pub(crate) enum FunctionNodeValue {
    /// A number or a bool, as the f64 a compiled entry carries.
    Number(f64),
    /// A `dict`, which only a node run as Python takes or gives.
    Record(Py<PyAny>),
    /// A message read off a channel, which a node hands on without reading it.
    MessageHandedOnUnread(Arc<MessageAFunctionNodeIslandHolds>),
    /// No value: a link from outside the island that carried none yet, a
    /// message input no new message reached, or an output its node handed out
    /// nothing on.
    Nothing,
}

impl FunctionNodeValue {
    fn as_a_number_for_an_input_of(&self, node_id: &str, input_index: usize) -> Result<f64> {
        match self {
            Self::Number(number) => Ok(*number),
            Self::Record(_) => Err(Error::Runtime(format!(
                "function node `{node_id}` runs compiled and takes a number on input \
                 {input_index}, and was handed a dict"
            ))),
            Self::MessageHandedOnUnread(_) => Err(Error::Runtime(format!(
                "function node `{node_id}` takes a number on input {input_index}, and was \
                 handed a message to hand on unread; only an `UnreadMessage` input takes one"
            ))),
            Self::Nothing => Err(Error::Runtime(format!(
                "function node `{node_id}` takes a number on input {input_index}, and no value \
                 reached it"
            ))),
        }
    }

    fn is_nothing(&self) -> bool {
        matches!(self, Self::Nothing)
    }

    fn duplicated(&self) -> Self {
        match self {
            Self::Number(number) => Self::Number(*number),
            Self::Record(record) => Python::attach(|python| Self::Record(record.clone_ref(python))),
            Self::MessageHandedOnUnread(message) => {
                Self::MessageHandedOnUnread(Arc::clone(message))
            }
            Self::Nothing => Self::Nothing,
        }
    }

    fn to_python<'py>(&self, python: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        Ok(match self {
            Self::Number(number) => number.into_pyobject(python)?.into_any(),
            Self::Record(record) => record.bind(python).clone(),
            Self::MessageHandedOnUnread(message) => Bound::new(
                python,
                UnreadMessageAFunctionNodeHandsOn {
                    message: Arc::clone(message),
                },
            )?
            .into_any(),
            Self::Nothing => python.None().into_bound(python),
        })
    }

    /// `value` as a function node gives it: `None` as nothing, a message it
    /// was handed as that message, a `dict` as a record, anything else as the
    /// number it converts to.
    fn from_python(value: &Bound<'_, PyAny>) -> PyResult<Self> {
        if value.is_none() {
            return Ok(Self::Nothing);
        }
        if let Ok(unread_message) = value.cast::<UnreadMessageAFunctionNodeHandsOn>() {
            return Ok(Self::MessageHandedOnUnread(Arc::clone(
                &unread_message.get().message,
            )));
        }
        if value.is_instance_of::<PyDict>() {
            return Ok(Self::Record(value.clone().unbind()));
        }
        Ok(Self::Number(value.extract::<f64>()?))
    }

    fn as_the_island_output_of(
        &self,
        node_id: &str,
        output_index: usize,
    ) -> Result<FunctionNodeIslandOutputValue> {
        Ok(match self {
            Self::Number(number) => FunctionNodeIslandOutputValue::ControlValue(*number),
            Self::Record(record) => Python::attach(|python| {
                records_message_payload_of(
                    &format!("function node `{node_id}`'s output {output_index}"),
                    record.bind(python),
                    &[],
                )
                .map(FunctionNodeIslandOutputValue::RecordsMessage)
                .map_err(|refusal| Error::Runtime(refusal.to_string()))
            })?,
            Self::MessageHandedOnUnread(message) => {
                FunctionNodeIslandOutputValue::MessageHandedOnUnread(message.message.clone())
            }
            Self::Nothing => FunctionNodeIslandOutputValue::Nothing,
        })
    }
}

/// What holds a pooled GPU frame against its producer's reuse while a message
/// an island holds names it; released as the message's last copy drops.
pub(crate) type PooledGpuFrameClaimAnIslandHolds = Box<dyn std::any::Any + Send + Sync>;

/// Claims the pooled GPU frame a message names as an island reads it off its
/// channel; `Ok(None)` where nothing in this helper covers the frame's pool.
pub(crate) type ClaimOfThePooledGpuFrameAMessageNames = Box<
    dyn Fn(
            GpuPoolSlotFrameDescriptorBody,
        ) -> std::result::Result<Option<PooledGpuFrameClaimAnIslandHolds>, String>
        + Send
        + Sync,
>;

/// A message an island read off a channel to hand on unread, holding the
/// pooled GPU frame it names, if any, for as long as any copy of it lives: in
/// an input, an output, or a node's state.
pub(crate) struct MessageAFunctionNodeIslandHolds {
    message: MessageAFunctionNodeIslandHandsOnUnread,
    _claim_of_its_pooled_gpu_frame: Option<PooledGpuFrameClaimAnIslandHolds>,
}

impl std::fmt::Debug for MessageAFunctionNodeIslandHolds {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MessageAFunctionNodeIslandHolds")
            .field("message", &self.message)
            .field(
                "holds_its_pooled_gpu_frame",
                &self._claim_of_its_pooled_gpu_frame.is_some(),
            )
            .finish()
    }
}

impl MessageAFunctionNodeIslandHolds {
    /// Holds `message`, claiming the pooled GPU frame it names, if any, through
    /// `claim_the_pooled_gpu_frame_a_message_names`.
    fn hold_a_message_read_off_its_channel(
        message: &MessageReadFromInputPort,
        claim_the_pooled_gpu_frame_a_message_names: &ClaimOfThePooledGpuFrameAMessageNames,
        input_port_it_was_read_at: &str,
    ) -> Self {
        let claim_of_its_pooled_gpu_frame = the_pooled_gpu_frame_a_message_names(
            &message.payload_descriptor,
        )
        .and_then(|pooled_gpu_frame| {
            claim_the_pooled_gpu_frame_a_message_names(pooled_gpu_frame).unwrap_or_else(|refusal| {
                tracing::warn!(
                    input_port = %input_port_it_was_read_at,
                    "the pooled GPU frame a message names could not be claimed, so \
                     its producer may reuse it while the message is held: {refusal}"
                );
                None
            })
        });
        Self {
            message: MessageAFunctionNodeIslandHandsOnUnread::read_off_its_channel(message),
            _claim_of_its_pooled_gpu_frame: claim_of_its_pooled_gpu_frame,
        }
    }
}

/// The pooled GPU frame a message names: an Image frame's slot, or a pooled
/// frame's; `None` for a message whose payload is its bytes.
fn the_pooled_gpu_frame_a_message_names(
    payload_descriptor: &DataChannelPayloadDescriptor,
) -> Option<GpuPoolSlotFrameDescriptorBody> {
    match DataChannelPayloadDescriptorKind::from_wire(payload_descriptor.kind)? {
        DataChannelPayloadDescriptorKind::ImageFrame => {
            Some(ImageFrameDescriptorBody::from_descriptor_body(&payload_descriptor.body).slot)
        }
        DataChannelPayloadDescriptorKind::GpuPoolSlotFrame => Some(
            GpuPoolSlotFrameDescriptorBody::from_descriptor_body(&payload_descriptor.body),
        ),
        _ => None,
    }
}

/// A message of any family a function node was handed to pass on without
/// reading it: the body can only hand it on, or keep it in its state.
#[pyclass(name = "UnreadMessage", module = "tatolab.stream", frozen)]
pub(crate) struct UnreadMessageAFunctionNodeHandsOn {
    message: Arc<MessageAFunctionNodeIslandHolds>,
}

#[pymethods]
impl UnreadMessageAFunctionNodeHandsOn {
    /// The message itself: it is immutable, so a copy is the same message.
    fn __copy__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    /// The message itself, as a state record's deep copy takes it.
    #[pyo3(signature = (memo))]
    fn __deepcopy__(slf: Py<Self>, #[allow(unused_variables)] memo: &Bound<'_, PyAny>) -> Py<Self> {
        slf
    }
}

/// How a function node input reads the messages a link from outside its
/// island carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HowAnInputReadsALinkFromOutsideItsIsland {
    /// As the control value a Channels message carries: its first sample.
    ControlValue,
    /// As the dict a Records message carries.
    Record,
    /// As the message itself, handed on unread: nothing at an evaluation no new
    /// message arrived before.
    MessageHandedOnUnread,
}

impl HowAnInputReadsALinkFromOutsideItsIsland {
    /// How an input of `value_type`, as the bootstrap names it, reads a link.
    fn of_the_value_type(value_type: &str) -> std::result::Result<Self, String> {
        match value_type {
            "float" | "int" | "bool" => Ok(Self::ControlValue),
            "records" => Ok(Self::Record),
            "unread_message" => Ok(Self::MessageHandedOnUnread),
            unknown => Err(format!(
                "an input of value type {unknown:?} cannot read a link from a node outside its \
                 island; it reads a number, a dict or an `UnreadMessage`"
            )),
        }
    }
}

/// The control value a Channels message carries: its first sample.
fn the_control_value_a_message_carries(
    message: &MessageReadFromInputPort,
    node_id: &str,
    reader_input_port: &str,
) -> Result<f64> {
    let refusal = |what_it_carries: &str| {
        Error::Runtime(format!(
            "function node `{node_id}` reads a number off the link it reads at \
             {reader_input_port:?}, and the message {what_it_carries}"
        ))
    };
    let descriptor = message.payload_descriptor;
    if DataChannelPayloadDescriptorKind::from_wire(descriptor.kind)
        != Some(DataChannelPayloadDescriptorKind::ChannelsBlock)
    {
        return Err(refusal("is no Channels block"));
    }
    let block = ChannelsBlockDescriptorBody::from_descriptor_body(&descriptor.body);
    let payload = message.payload.bytes();
    match ChannelsBlockSampleType::from_wire(block.sample_type) {
        Some(ChannelsBlockSampleType::F64) => payload
            .get(..8)
            .map(|sample| f64::from_le_bytes(sample.try_into().unwrap_or_default()))
            .ok_or_else(|| refusal("carries no sample")),
        Some(ChannelsBlockSampleType::F32) => payload
            .get(..4)
            .map(|sample| f64::from(f32::from_le_bytes(sample.try_into().unwrap_or_default())))
            .ok_or_else(|| refusal("carries no sample")),
        None => Err(refusal("names a sample type this build does not know")),
    }
}

/// Reads the newest message at one of the reader's input ports, past any older
/// ones still queued there; `None` when none arrived since the last read.
type NewestMessageReaderOfTheReadersInputPorts =
    Box<dyn Fn(&str) -> Result<Option<MessageReadFromInputPort>> + Send + Sync>;

/// The most messages one read of the newest message takes off a port, so a
/// publisher that keeps up with the reads cannot hold an evaluation in the
/// loop; any past it wait for the next read.
const MOST_MESSAGES_ONE_READ_OF_THE_NEWEST_TAKES: usize = 64;

fn the_newest_message_read_from(
    input_mailboxes: &InputMailboxesInner,
    reader_input_port: &str,
) -> Result<Option<MessageReadFromInputPort>> {
    let mut newest: Option<MessageReadFromInputPort> = None;
    for _ in 0..MOST_MESSAGES_ONE_READ_OF_THE_NEWEST_TAKES {
        let Some(mut message) = input_mailboxes.read_raw(reader_input_port)? else {
            break;
        };
        if let Some(replaced) = newest {
            message.lost_since_last_read = message
                .lost_since_last_read
                .saturating_add(replaced.lost_since_last_read);
        }
        newest = Some(message);
    }
    Ok(newest)
}

/// What the bootstrap says of how one function node's values travel beyond
/// numbers: the inputs it reads off links from outside its island, those that
/// take `None`, and the inputs and outputs that carry a message handed on
/// unread or may carry nothing.
///
/// Wire contract with the helper bootstrap: a dict of these five keys, the
/// first `[(input_index, reader_input_port, value_type)]`, the rest input or
/// output indices.
#[derive(Debug, Default, FromPyObject)]
pub(crate) struct HowAFunctionNodeCarriesValuesBeyondNumbers {
    #[pyo3(item)]
    inputs_read_from_links_from_outside_the_island: Vec<(usize, String, String)>,
    #[pyo3(item)]
    inputs_that_take_none: Vec<usize>,
    #[pyo3(item)]
    inputs_carrying_an_unread_message: Vec<usize>,
    #[pyo3(item)]
    outputs_carrying_an_unread_message: Vec<usize>,
    #[pyo3(item)]
    outputs_that_may_hand_out_nothing: Vec<usize>,
}

/// How an island reads values beyond numbers: what each of its nodes carries,
/// in island order, and how the newest message at a reader input port is read.
pub(crate) struct HowAFunctionNodeIslandCarriesValuesBeyondNumbers {
    how_each_node_carries_its_values: Vec<HowAFunctionNodeCarriesValuesBeyondNumbers>,
    read_the_newest_message_at: NewestMessageReaderOfTheReadersInputPorts,
    values_last_read_off_links_from_outside_islands: ValuesLastReadOffLinksFromOutsideIslands,
    claim_the_pooled_gpu_frame_a_message_names: ClaimOfThePooledGpuFrameAMessageNames,
}

/// The control value or record each reader input port last carried into an
/// island this helper evaluates, which an island replacing it starts from.
pub(crate) type ValuesLastReadOffLinksFromOutsideIslands =
    Arc<Mutex<HashMap<String, FunctionNodeValue>>>;

impl HowAFunctionNodeIslandCarriesValuesBeyondNumbers {
    /// An island whose every value is a number, a dict or a pin.
    #[cfg(test)]
    fn none() -> Self {
        Self {
            how_each_node_carries_its_values: Vec::new(),
            read_the_newest_message_at: Box::new(|_| Ok(None)),
            values_last_read_off_links_from_outside_islands: Arc::default(),
            claim_the_pooled_gpu_frame_a_message_names: Box::new(|_| Ok(None)),
        }
    }
}

/// Where one input of a function node in an island takes its value from.
#[derive(Debug)]
enum WhereAFunctionNodeInputTakesItsValue {
    /// An output of a node evaluated earlier in the island.
    AnEarlierNodesOutput {
        node_index: usize,
        output_index: usize,
    },
    /// An output of a node evaluated later in the island, as it was at the
    /// last evaluation: how a `PreviousValue` node reads the loop it closes.
    ALaterNodesOutputAtTheLastEvaluation {
        node_index: usize,
        output_index: usize,
    },
    /// The pin's value, as nothing is linked to it.
    ItsPin(FunctionNodeValue),
    /// A link from a node outside the island, whose channel the reader reads at
    /// `reader_input_port`: the value read off it last.
    ALinkFromOutsideTheIsland {
        reader_input_port: String,
        read_as: HowAnInputReadsALinkFromOutsideItsIsland,
        value_read_last: FunctionNodeValue,
    },
}

/// A function node's evaluate entry, compiled for its signature, and the f64
/// buffers it is called with.
struct FunctionNodeCalledThroughItsEvaluateEntry {
    evaluate_entry: ModuleHostTableFunctionNodeEvaluateEntry,
    /// The values a call's message inputs and outputs carry as tokens: a
    /// message input as its index plus one, or 0 for none; and the slot past
    /// the outputs where each output that may hand out nothing says whether it
    /// handed one out.
    values_carried_as_tokens: FunctionNodeValuesCarriedAsTokensThroughAnEvaluateEntry,
    config_values: Vec<f64>,
    /// The index among `config_values` each input past the node's own sets: a
    /// config field it exposes as an input, linked.
    config_value_indices_set_by_the_inputs_past_its_own: Vec<usize>,
    input_values: Vec<f64>,
    state_values: Vec<f64>,
    state_values_of_the_call_under_way: Vec<f64>,
    output_values: Vec<f64>,
}

/// The inputs and outputs of a compiled call that carry a message as a token,
/// and the slot of each output's handed-out flag.
///
/// ABI contract with the evaluate entry the bootstrap compiles: a message input
/// carries `input_index + 1`, or `0.0` when it holds none; a message output
/// carries the token of the input it hands on, or `0.0`; and each output that
/// may hand out nothing has a slot past every output, in output order, holding
/// `1.0` when it handed one out and `0.0` when it did not.
#[derive(Debug, Default)]
struct FunctionNodeValuesCarriedAsTokensThroughAnEvaluateEntry {
    inputs_carrying_an_unread_message: Vec<usize>,
    outputs_carrying_an_unread_message: Vec<usize>,
    handed_out_slot_of_each_output: Vec<Option<usize>>,
}

/// A function node whose body runs as Python in this helper's interpreter,
/// through the object the bootstrap built for it.
struct FunctionNodeCalledAsPython {
    input_count: usize,
    /// Answers `evaluate(input_values, now_ns)` with its outputs in port order,
    /// keeping its state, and `state_values()` with that state as f64s when
    /// its state record is numbers and bools.
    run_as_python: Py<PyAny>,
    /// The state it last answered, as written on the board.
    state_values: Vec<f64>,
}

/// How one function node is called.
enum HowAFunctionNodeIsCalled {
    ThroughItsEvaluateEntry(FunctionNodeCalledThroughItsEvaluateEntry),
    AsPython(FunctionNodeCalledAsPython),
}

/// One function node of an island, and the values it is called with.
struct FunctionNodeInAnIsland {
    node_id: String,
    breadcrumb_number: u32,
    how_it_is_called: HowAFunctionNodeIsCalled,
    where_each_input_takes_its_value: Vec<WhereAFunctionNodeInputTakesItsValue>,
    /// The inputs a call may leave with no value, handing the body `None`; at
    /// any other input with no value the node hands out nothing, uncalled.
    inputs_that_take_none: Vec<usize>,
    output_values: Vec<FunctionNodeValue>,
    /// Whether each call's new state is written on the helper's board, which
    /// carries an entry for the node exactly when its state is carried.
    writes_its_state_on_the_board: bool,
}

/// The values every node of an island last took, and the stream time they were
/// evaluated at.
struct FunctionNodeIslandEvaluation {
    function_nodes_each_after_what_it_reads: Vec<FunctionNodeInAnIsland>,
    last_evaluated_at_ns: Option<i64>,
    /// Whether the island output's message, when it hands one on, went to a
    /// read since the last evaluation; it goes to one read only.
    message_handed_on_since_the_last_evaluation: bool,
    read_the_newest_message_at: NewestMessageReaderOfTheReadersInputPorts,
    values_last_read_off_links_from_outside_islands: ValuesLastReadOffLinksFromOutsideIslands,
    claim_the_pooled_gpu_frame_a_message_names: ClaimOfThePooledGpuFrameAMessageNames,
    /// The board each call's breadcrumb is noted on, so the parent can name
    /// the node a call died or stalled in.
    call_breadcrumb_board_writer: Option<Arc<HelperProcessLossCountBoardWriter>>,
}

/// A function node island this helper evaluates at each read of the port it
/// feeds.
pub(crate) struct FunctionNodeIslandEvaluatedInThisHelper {
    island_output_node_index: usize,
    island_output_index: usize,
    first_function_node_it_runs_as_python: Option<String>,
    reader_input_ports_its_links_from_outside_it_arrive_at: Vec<String>,
    evaluation: Mutex<FunctionNodeIslandEvaluation>,
    stream_clock_now_ns: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl FunctionNodeIslandEvaluation {
    /// Read each link from outside the island at its reader input port: a
    /// number or a dict keeps the newest value it carried, and a message input
    /// holds the message that arrived since the last evaluation, or nothing.
    fn read_each_link_from_outside_the_island(&mut self) -> Result<()> {
        for function_node in &mut self.function_nodes_each_after_what_it_reads {
            for where_it_takes_its_value in &mut function_node.where_each_input_takes_its_value {
                let WhereAFunctionNodeInputTakesItsValue::ALinkFromOutsideTheIsland {
                    reader_input_port,
                    read_as,
                    value_read_last,
                } = where_it_takes_its_value
                else {
                    continue;
                };
                let newest_message = (self.read_the_newest_message_at)(reader_input_port)?;
                match (newest_message, *read_as) {
                    (Some(message), HowAnInputReadsALinkFromOutsideItsIsland::ControlValue) => {
                        *value_read_last =
                            FunctionNodeValue::Number(the_control_value_a_message_carries(
                                &message,
                                &function_node.node_id,
                                reader_input_port,
                            )?);
                        self.values_last_read_off_links_from_outside_islands
                            .lock()
                            .insert(reader_input_port.clone(), value_read_last.duplicated());
                    }
                    (Some(message), HowAnInputReadsALinkFromOutsideItsIsland::Record) => {
                        *value_read_last = Python::attach(|python| {
                            records_message_dict_of(
                                python,
                                reader_input_port,
                                message.payload.bytes(),
                            )
                            .map(|record| FunctionNodeValue::Record(record.unbind()))
                            .map_err(|refusal| Error::Runtime(refusal.to_string()))
                        })?;
                        self.values_last_read_off_links_from_outside_islands
                            .lock()
                            .insert(reader_input_port.clone(), value_read_last.duplicated());
                    }
                    (
                        Some(message),
                        HowAnInputReadsALinkFromOutsideItsIsland::MessageHandedOnUnread,
                    ) => {
                        *value_read_last = FunctionNodeValue::MessageHandedOnUnread(Arc::new(
                            MessageAFunctionNodeIslandHolds::hold_a_message_read_off_its_channel(
                                &message,
                                &self.claim_the_pooled_gpu_frame_a_message_names,
                                reader_input_port,
                            ),
                        ));
                    }
                    (None, HowAnInputReadsALinkFromOutsideItsIsland::MessageHandedOnUnread) => {
                        *value_read_last = FunctionNodeValue::Nothing;
                    }
                    (None, _) => {}
                }
            }
        }
        Ok(())
    }

    fn evaluate_every_node_at(&mut self, now_ns: i64) -> Result<()> {
        self.read_each_link_from_outside_the_island()?;
        self.message_handed_on_since_the_last_evaluation = false;
        for node_index in 0..self.function_nodes_each_after_what_it_reads.len() {
            let (evaluated_earlier, from_this_node_on) = self
                .function_nodes_each_after_what_it_reads
                .split_at_mut(node_index);
            let Some((function_node, evaluated_later)) = from_this_node_on.split_first_mut() else {
                break;
            };
            let input_values: Vec<&FunctionNodeValue> = function_node
                .where_each_input_takes_its_value
                .iter()
                .map(|where_it_takes_its_value| {
                    match where_it_takes_its_value {
                    WhereAFunctionNodeInputTakesItsValue::AnEarlierNodesOutput {
                        node_index,
                        output_index,
                    } => &evaluated_earlier[*node_index].output_values[*output_index],
                    WhereAFunctionNodeInputTakesItsValue::ALaterNodesOutputAtTheLastEvaluation {
                        node_index: later_node_index,
                        output_index,
                    } => {
                        &evaluated_later[*later_node_index - node_index - 1].output_values
                            [*output_index]
                    }
                    WhereAFunctionNodeInputTakesItsValue::ItsPin(pin_value) => pin_value,
                    WhereAFunctionNodeInputTakesItsValue::ALinkFromOutsideTheIsland {
                        value_read_last,
                        ..
                    } => value_read_last,
                }
                })
                .collect();
            let an_input_that_needs_a_value_has_none =
                input_values
                    .iter()
                    .enumerate()
                    .any(|(input_index, input_value)| {
                        input_value.is_nothing()
                            && !function_node.inputs_that_take_none.contains(&input_index)
                    });
            if an_input_that_needs_a_value_has_none {
                for output_value in &mut function_node.output_values {
                    *output_value = FunctionNodeValue::Nothing;
                }
                continue;
            }
            call_noting_its_breadcrumb_on(
                self.call_breadcrumb_board_writer.as_deref(),
                &function_node.node_id,
                function_node.breadcrumb_number,
                &mut function_node.how_it_is_called,
                &mut function_node.output_values,
                &input_values,
                now_ns,
            )?;
            if let Some(board_writer) = self.call_breadcrumb_board_writer.as_deref()
                && function_node.writes_its_state_on_the_board
            {
                board_writer.write_the_state_of_a_function_node(
                    function_node.breadcrumb_number,
                    function_node.how_it_is_called.state_values(),
                )?;
            }
        }
        self.last_evaluated_at_ns = Some(now_ns);
        Ok(())
    }
}

/// Call a function node at `now_ns` with `input_values`, its breadcrumb noted
/// on `call_breadcrumb_board_writer` for the length of the call.
fn call_noting_its_breadcrumb_on(
    call_breadcrumb_board_writer: Option<&HelperProcessLossCountBoardWriter>,
    node_id: &str,
    breadcrumb_number: u32,
    how_it_is_called: &mut HowAFunctionNodeIsCalled,
    output_values: &mut [FunctionNodeValue],
    input_values: &[&FunctionNodeValue],
    now_ns: i64,
) -> Result<()> {
    let call_under_way = call_breadcrumb_board_writer.map(|board_writer| {
        board_writer.note_a_function_node_call_started(breadcrumb_number, monotonic_clock_now_ns())
    });
    let called = how_it_is_called.call_at(node_id, input_values, output_values, now_ns);
    if let (Some(board_writer), Some(call_under_way)) =
        (call_breadcrumb_board_writer, call_under_way)
    {
        board_writer.note_the_function_node_call_returned(call_under_way);
    }
    called
}

/// The state a function node starts its first call from: the state its stream
/// carries for it when that is one of the same length, its state record's
/// defaults otherwise.
fn the_state_a_function_node_starts_from(
    initial_state_values: Vec<f64>,
    state_resumed_from: Option<Vec<f64>>,
) -> Vec<f64> {
    match state_resumed_from {
        Some(state_resumed_from) if state_resumed_from.len() == initial_state_values.len() => {
            state_resumed_from
        }
        _ => initial_state_values,
    }
}

impl HowAFunctionNodeIsCalled {
    /// How the bootstrap hands a node over: its compiled entry's address, or
    /// the object that runs its body as Python. A node run as Python is told
    /// the state its stream carries for it, when it carries one. Of a compiled
    /// node's `input_count` inputs, the last set the config values
    /// `config_value_indices_set_by_the_inputs_past_its_own` names.
    #[allow(clippy::too_many_arguments)]
    fn handed_over_by_the_bootstrap(
        node_id: &str,
        called_through: Py<PyAny>,
        config_values: Vec<f64>,
        config_value_indices_set_by_the_inputs_past_its_own: Vec<usize>,
        input_count: usize,
        state_values: Vec<f64>,
        resumes_from_the_state_its_stream_carries: bool,
        output_count: usize,
        how_it_carries_its_values: &HowAFunctionNodeCarriesValuesBeyondNumbers,
    ) -> std::result::Result<Self, String> {
        Python::attach(|python| {
            let called_through = called_through.bind(python);
            if called_through.is_instance_of::<PyInt>() {
                let evaluate_entry_address: usize = called_through
                    .extract()
                    .map_err(|refusal| format!("function node `{node_id}`: {refusal}"))?;
                if evaluate_entry_address == 0 {
                    return Err(format!(
                        "function node `{node_id}` was handed over with no evaluate entry"
                    ));
                }
                // SAFETY: the bootstrap hands over the address of an entry it
                // compiled to this signature and keeps alive for as long as
                // this helper reads, and the address was refused above when
                // null.
                let evaluate_entry = unsafe {
                    std::mem::transmute::<usize, ModuleHostTableFunctionNodeEvaluateEntry>(
                        evaluate_entry_address,
                    )
                };
                if let Some(config_value_index) =
                    config_value_indices_set_by_the_inputs_past_its_own
                        .iter()
                        .find(|config_value_index| **config_value_index >= config_values.len())
                {
                    return Err(format!(
                        "function node `{node_id}` sets config value {config_value_index} from \
                         an input, and it takes {} config values",
                        config_values.len()
                    ));
                }
                let own_input_count = input_count
                    .checked_sub(config_value_indices_set_by_the_inputs_past_its_own.len())
                    .ok_or_else(|| {
                        format!(
                            "function node `{node_id}` sets {} config values from inputs, and \
                             it takes {input_count} inputs",
                            config_value_indices_set_by_the_inputs_past_its_own.len()
                        )
                    })?;
                let values_carried_as_tokens =
                    FunctionNodeValuesCarriedAsTokensThroughAnEvaluateEntry::of(
                        node_id,
                        how_it_carries_its_values,
                        own_input_count,
                        output_count,
                    )?;
                let output_slot_count = output_count
                    + how_it_carries_its_values
                        .outputs_that_may_hand_out_nothing
                        .len();
                return Ok(Self::ThroughItsEvaluateEntry(
                    FunctionNodeCalledThroughItsEvaluateEntry {
                        evaluate_entry,
                        values_carried_as_tokens,
                        config_values,
                        config_value_indices_set_by_the_inputs_past_its_own,
                        input_values: vec![0.0; own_input_count],
                        state_values_of_the_call_under_way: vec![0.0; state_values.len()],
                        state_values,
                        output_values: vec![0.0; output_slot_count],
                    },
                ));
            }
            if resumes_from_the_state_its_stream_carries {
                called_through
                    .call_method1("resume_from_state_values", (state_values.clone(),))
                    .map_err(|refusal| {
                        format!(
                            "function node `{node_id}` did not resume from the state its \
                             stream carries: {refusal}"
                        )
                    })?;
            }
            Ok(Self::AsPython(FunctionNodeCalledAsPython {
                input_count,
                run_as_python: called_through.clone().unbind(),
                state_values,
            }))
        })
    }

    fn input_count(&self) -> usize {
        match self {
            Self::ThroughItsEvaluateEntry(compiled) => {
                compiled.input_values.len()
                    + compiled
                        .config_value_indices_set_by_the_inputs_past_its_own
                        .len()
            }
            Self::AsPython(run_as_python) => run_as_python.input_count,
        }
    }

    fn runs_as_python(&self) -> bool {
        matches!(self, Self::AsPython(_))
    }

    fn state_values(&self) -> &[f64] {
        match self {
            Self::ThroughItsEvaluateEntry(compiled) => &compiled.state_values,
            Self::AsPython(run_as_python) => &run_as_python.state_values,
        }
    }

    /// Call the node with `input_values`, writing `output_values` and keeping
    /// the new state only when the call succeeds.
    fn call_at(
        &mut self,
        node_id: &str,
        input_values: &[&FunctionNodeValue],
        output_values: &mut [FunctionNodeValue],
        now_ns: i64,
    ) -> Result<()> {
        match self {
            Self::ThroughItsEvaluateEntry(compiled) => {
                let own_input_count = compiled.input_values.len();
                for (input_index, input_value) in input_values.iter().enumerate() {
                    let input_number = if compiled
                        .values_carried_as_tokens
                        .inputs_carrying_an_unread_message
                        .contains(&input_index)
                    {
                        if input_value.is_nothing() {
                            0.0
                        } else {
                            (input_index + 1) as f64
                        }
                    } else {
                        input_value.as_a_number_for_an_input_of(node_id, input_index)?
                    };
                    match input_index.checked_sub(own_input_count) {
                        None => compiled.input_values[input_index] = input_number,
                        Some(past_its_own) => {
                            if let Some(config_value_index) = compiled
                                .config_value_indices_set_by_the_inputs_past_its_own
                                .get(past_its_own)
                            {
                                compiled.config_values[*config_value_index] = input_number;
                            }
                        }
                    }
                }
                compiled.call_its_evaluate_entry_at(node_id, now_ns)?;
                let values_the_outputs_took = (0..output_values.len())
                    .map(|output_index| {
                        compiled.the_value_an_output_took(node_id, output_index, input_values)
                    })
                    .collect::<Result<Vec<FunctionNodeValue>>>()?;
                compiled.keep_the_state_of_the_call_just_made();
                for (output_value, value_the_output_took) in
                    output_values.iter_mut().zip(values_the_outputs_took)
                {
                    *output_value = value_the_output_took;
                }
                Ok(())
            }
            Self::AsPython(run_as_python) => {
                run_as_python.call_at(node_id, input_values, output_values, now_ns)
            }
        }
    }
}

impl FunctionNodeValuesCarriedAsTokensThroughAnEvaluateEntry {
    fn of(
        node_id: &str,
        how_it_carries_its_values: &HowAFunctionNodeCarriesValuesBeyondNumbers,
        own_input_count: usize,
        output_count: usize,
    ) -> std::result::Result<Self, String> {
        let refusal = |what: &str, index: usize, count: usize| {
            format!(
                "function node `{node_id}` names {what} {index} as carrying a message or \
                 nothing, and it has {count}"
            )
        };
        for input_index in &how_it_carries_its_values.inputs_carrying_an_unread_message {
            if *input_index >= own_input_count {
                return Err(refusal("input", *input_index, own_input_count));
            }
        }
        for output_index in how_it_carries_its_values
            .outputs_carrying_an_unread_message
            .iter()
            .chain(&how_it_carries_its_values.outputs_that_may_hand_out_nothing)
        {
            if *output_index >= output_count {
                return Err(refusal("output", *output_index, output_count));
            }
        }
        let mut handed_out_slot_of_each_output = vec![None; output_count];
        for (handed_out_slot_index, output_index) in how_it_carries_its_values
            .outputs_that_may_hand_out_nothing
            .iter()
            .enumerate()
        {
            handed_out_slot_of_each_output[*output_index] =
                Some(output_count + handed_out_slot_index);
        }
        Ok(Self {
            inputs_carrying_an_unread_message: how_it_carries_its_values
                .inputs_carrying_an_unread_message
                .clone(),
            outputs_carrying_an_unread_message: how_it_carries_its_values
                .outputs_carrying_an_unread_message
                .clone(),
            handed_out_slot_of_each_output,
        })
    }
}

impl FunctionNodeCalledThroughItsEvaluateEntry {
    /// The value output `output_index` took at the call just made: nothing
    /// when its handed-out slot says so, the input its token names for a
    /// message output, and the number it wrote otherwise.
    fn the_value_an_output_took(
        &self,
        node_id: &str,
        output_index: usize,
        input_values: &[&FunctionNodeValue],
    ) -> Result<FunctionNodeValue> {
        let tokens = &self.values_carried_as_tokens;
        let handed_one_out = tokens
            .handed_out_slot_of_each_output
            .get(output_index)
            .copied()
            .flatten()
            .is_none_or(|handed_out_slot| self.output_values[handed_out_slot] != 0.0);
        if !handed_one_out {
            return Ok(FunctionNodeValue::Nothing);
        }
        let written = self.output_values[output_index];
        if !tokens
            .outputs_carrying_an_unread_message
            .contains(&output_index)
        {
            return Ok(FunctionNodeValue::Number(written));
        }
        if written == 0.0 {
            return Ok(FunctionNodeValue::Nothing);
        }
        let named_input_index = (written - 1.0) as usize;
        let names_a_message_input = written.fract() == 0.0
            && written >= 1.0
            && tokens
                .inputs_carrying_an_unread_message
                .contains(&named_input_index);
        match input_values.get(named_input_index) {
            Some(input_value) if names_a_message_input => Ok(input_value.duplicated()),
            _ => Err(Error::Runtime(format!(
                "function node `{node_id}` handed out {written} on output {output_index}, which \
                 carries a message it was handed, and names none of its message inputs"
            ))),
        }
    }

    /// Call the node's evaluate entry, writing the state it answers aside
    /// until [`Self::keep_the_state_of_the_call_just_made`].
    fn call_its_evaluate_entry_at(&mut self, node_id: &str, now_ns: i64) -> Result<()> {
        self.state_values_of_the_call_under_way
            .copy_from_slice(&self.state_values);
        // SAFETY: the entry was compiled for this node's signature, so it reads
        // exactly `input_values.len()` inputs and `config_values.len()` config
        // values and writes at most `state_values.len()` state values and
        // `output_values.len()` outputs, each buffer that long and live across
        // the call.
        let status = unsafe {
            (self.evaluate_entry)(
                self.input_values.as_ptr(),
                self.config_values.as_ptr(),
                self.state_values_of_the_call_under_way.as_mut_ptr(),
                self.output_values.as_mut_ptr(),
                now_ns,
            )
        };
        if status != MODULE_HOST_TABLE_STATUS_OK {
            return Err(Error::Runtime(format!(
                "function node `{node_id}` refused its evaluation with status {status}"
            )));
        }
        Ok(())
    }

    /// Keep the state the last call wrote, once every output it handed out is
    /// one the island takes.
    fn keep_the_state_of_the_call_just_made(&mut self) {
        std::mem::swap(
            &mut self.state_values,
            &mut self.state_values_of_the_call_under_way,
        );
    }
}

impl FunctionNodeCalledAsPython {
    /// Call the node's body as Python, holding the interpreter's lock for the
    /// length of the call.
    fn call_at(
        &mut self,
        node_id: &str,
        input_values: &[&FunctionNodeValue],
        output_values: &mut [FunctionNodeValue],
        now_ns: i64,
    ) -> Result<()> {
        Python::attach(|python| {
            let raised = |refusal: PyErr| {
                Error::Runtime(format!(
                    "function node `{node_id}`, run as Python, raised: {refusal}"
                ))
            };
            let input_objects = input_values
                .iter()
                .map(|input_value| input_value.to_python(python))
                .collect::<PyResult<Vec<_>>>()
                .map_err(raised)?;
            let input_list = PyList::new(python, input_objects).map_err(raised)?;
            let returned = self
                .run_as_python
                .bind(python)
                .call_method1("evaluate", (input_list, now_ns))
                .map_err(raised)?;
            let returned: Vec<Bound<'_, PyAny>> = returned.extract().map_err(raised)?;
            if returned.len() != output_values.len() {
                return Err(Error::Runtime(format!(
                    "function node `{node_id}`, run as Python, answered {} outputs, and it has {}",
                    returned.len(),
                    output_values.len()
                )));
            }
            let mut outputs_given = Vec::with_capacity(returned.len());
            for output in &returned {
                outputs_given.push(FunctionNodeValue::from_python(output).map_err(raised)?);
            }
            if !self.state_values.is_empty() {
                self.state_values = self
                    .run_as_python
                    .bind(python)
                    .call_method0("state_values")
                    .and_then(|state_values| state_values.extract())
                    .map_err(raised)?;
            }
            for (output_value, output_given) in output_values.iter_mut().zip(outputs_given) {
                *output_value = output_given;
            }
            Ok(())
        })
    }
}

impl FunctionNodeIslandEvaluatedAtEachRead for FunctionNodeIslandEvaluatedInThisHelper {
    fn evaluate_the_island_output_for_this_read(
        &self,
    ) -> Result<FunctionNodeIslandOutputForOneRead> {
        let mut evaluation = self.evaluation.lock();
        let now_ns = (self.stream_clock_now_ns)();
        if evaluation.last_evaluated_at_ns != Some(now_ns) {
            evaluation.evaluate_every_node_at(now_ns)?;
        }
        let island_output_node =
            &evaluation.function_nodes_each_after_what_it_reads[self.island_output_node_index];
        let value = island_output_node.output_values[self.island_output_index]
            .as_the_island_output_of(&island_output_node.node_id, self.island_output_index)?;
        let value = match value {
            FunctionNodeIslandOutputValue::MessageHandedOnUnread(_)
                if evaluation.message_handed_on_since_the_last_evaluation =>
            {
                FunctionNodeIslandOutputValue::Nothing
            }
            FunctionNodeIslandOutputValue::MessageHandedOnUnread(message) => {
                evaluation.message_handed_on_since_the_last_evaluation = true;
                FunctionNodeIslandOutputValue::MessageHandedOnUnread(message)
            }
            value => value,
        };
        Ok(FunctionNodeIslandOutputForOneRead {
            value,
            evaluated_at_ns: now_ns,
        })
    }

    fn the_first_function_node_it_runs_as_python(&self) -> Option<&str> {
        self.first_function_node_it_runs_as_python.as_deref()
    }

    fn the_state_each_stateful_function_node_last_returned(&self) -> Vec<(u32, Vec<f64>)> {
        self.evaluation
            .lock()
            .function_nodes_each_after_what_it_reads
            .iter()
            .filter(|function_node| !function_node.how_it_is_called.state_values().is_empty())
            .map(|function_node| {
                (
                    function_node.breadcrumb_number,
                    function_node.how_it_is_called.state_values().to_vec(),
                )
            })
            .collect()
    }

    fn the_reader_input_ports_its_links_from_outside_it_arrive_at(&self) -> &[String] {
        &self.reader_input_ports_its_links_from_outside_it_arrive_at
    }
}

/// One function node as the bootstrap hands it over: its id, its breadcrumb
/// number, its compiled evaluate entry's address or the object that runs it as
/// Python, where each input takes its value, its config values, its initial
/// state values, how many outputs it writes, and the index among its config
/// values each input past its own sets.
type FunctionNodeHandedOverByTheBootstrap = (
    String,
    u32,
    Py<PyAny>,
    Vec<(Option<(usize, usize)>, f64)>,
    Vec<f64>,
    Vec<f64>,
    usize,
    Vec<usize>,
);

impl FunctionNodeIslandEvaluatedInThisHelper {
    fn of_the_function_nodes_the_bootstrap_compiled(
        function_nodes: Vec<FunctionNodeHandedOverByTheBootstrap>,
        (island_output_node_index, island_output_index): (usize, usize),
        stream_clock_now_ns: Box<dyn Fn() -> i64 + Send + Sync>,
        call_breadcrumb_board_writer: Option<Arc<HelperProcessLossCountBoardWriter>>,
        state_resumed_from: &dyn Fn(u32) -> Option<Vec<f64>>,
        how_it_carries_values_beyond_numbers: HowAFunctionNodeIslandCarriesValuesBeyondNumbers,
    ) -> std::result::Result<Self, String> {
        let HowAFunctionNodeIslandCarriesValuesBeyondNumbers {
            mut how_each_node_carries_its_values,
            read_the_newest_message_at,
            values_last_read_off_links_from_outside_islands,
            claim_the_pooled_gpu_frame_a_message_names,
        } = how_it_carries_values_beyond_numbers;
        how_each_node_carries_its_values.resize_with(function_nodes.len(), Default::default);
        let mut function_nodes_each_after_what_it_reads: Vec<FunctionNodeInAnIsland> =
            Vec::with_capacity(function_nodes.len());
        for (
            node_index,
            (
                node_id,
                breadcrumb_number,
                called_through,
                where_each_input_takes_its_value,
                config_values,
                initial_state_values,
                output_count,
                config_value_indices_set_by_the_inputs_past_its_own,
            ),
        ) in function_nodes.into_iter().enumerate()
        {
            let how_it_carries_its_values = &how_each_node_carries_its_values[node_index];
            let mut where_each_input_takes_its_value_read = Vec::new();
            for (input_index, (source, pin_value)) in
                where_each_input_takes_its_value.into_iter().enumerate()
            {
                where_each_input_takes_its_value_read.push(match source {
                    None if how_it_carries_its_values
                        .inputs_carrying_an_unread_message
                        .contains(&input_index) =>
                    {
                        WhereAFunctionNodeInputTakesItsValue::ItsPin(FunctionNodeValue::Nothing)
                    }
                    None => WhereAFunctionNodeInputTakesItsValue::ItsPin(
                        FunctionNodeValue::Number(pin_value),
                    ),
                    Some((source_node_index, output_index)) if source_node_index > node_index => {
                        WhereAFunctionNodeInputTakesItsValue::ALaterNodesOutputAtTheLastEvaluation {
                            node_index: source_node_index,
                            output_index,
                        }
                    }
                    Some((source_node_index, output_index)) => {
                        let reads_an_output_evaluated_earlier = source_node_index < node_index
                            && output_index
                                < function_nodes_each_after_what_it_reads
                                    .get(source_node_index)
                                    .map_or(0, |earlier| earlier.output_values.len());
                        if !reads_an_output_evaluated_earlier {
                            return Err(format!(
                                "function node `{node_id}` reads output {output_index} of node \
                                 {source_node_index}, which is not an output of a node \
                                 evaluated before it"
                            ));
                        }
                        WhereAFunctionNodeInputTakesItsValue::AnEarlierNodesOutput {
                            node_index: source_node_index,
                            output_index,
                        }
                    }
                });
            }
            for (input_index, reader_input_port, value_type) in
                &how_it_carries_its_values.inputs_read_from_links_from_outside_the_island
            {
                let read_as =
                    HowAnInputReadsALinkFromOutsideItsIsland::of_the_value_type(value_type)
                        .map_err(|refusal| format!("function node `{node_id}`: {refusal}"))?;
                let where_it_takes_its_value = where_each_input_takes_its_value_read
                    .get_mut(*input_index)
                    .ok_or_else(|| {
                        format!(
                            "function node `{node_id}` reads input {input_index} off a link from \
                             outside its island, and it takes fewer inputs"
                        )
                    })?;
                let value_read_last = match read_as {
                    HowAnInputReadsALinkFromOutsideItsIsland::MessageHandedOnUnread => {
                        FunctionNodeValue::Nothing
                    }
                    _ => values_last_read_off_links_from_outside_islands
                        .lock()
                        .get(reader_input_port)
                        .map_or(FunctionNodeValue::Nothing, FunctionNodeValue::duplicated),
                };
                *where_it_takes_its_value =
                    WhereAFunctionNodeInputTakesItsValue::ALinkFromOutsideTheIsland {
                        reader_input_port: reader_input_port.clone(),
                        read_as,
                        value_read_last,
                    };
            }
            let writes_its_state_on_the_board = !initial_state_values.is_empty()
                && call_breadcrumb_board_writer
                    .as_deref()
                    .is_some_and(|board_writer| {
                        board_writer.carries_a_state_entry_for(breadcrumb_number)
                    });
            let state_resumed_from = state_resumed_from(breadcrumb_number)
                .filter(|resumed| resumed.len() == initial_state_values.len());
            let resumes_from_the_state_its_stream_carries =
                state_resumed_from.is_some() && !initial_state_values.is_empty();
            let how_it_is_called = HowAFunctionNodeIsCalled::handed_over_by_the_bootstrap(
                &node_id,
                called_through,
                config_values,
                config_value_indices_set_by_the_inputs_past_its_own,
                where_each_input_takes_its_value_read.len(),
                the_state_a_function_node_starts_from(initial_state_values, state_resumed_from),
                resumes_from_the_state_its_stream_carries,
                output_count,
                how_it_carries_its_values,
            )?;
            function_nodes_each_after_what_it_reads.push(FunctionNodeInAnIsland {
                node_id,
                breadcrumb_number,
                how_it_is_called,
                where_each_input_takes_its_value: where_each_input_takes_its_value_read,
                inputs_that_take_none: how_it_carries_its_values.inputs_that_take_none.clone(),
                output_values: (0..output_count)
                    .map(|_| FunctionNodeValue::Number(0.0))
                    .collect(),
                writes_its_state_on_the_board,
            });
        }
        for function_node in &function_nodes_each_after_what_it_reads {
            for where_it_takes_its_value in &function_node.where_each_input_takes_its_value {
                if let WhereAFunctionNodeInputTakesItsValue::ALaterNodesOutputAtTheLastEvaluation {
                    node_index: later_node_index,
                    output_index,
                } = where_it_takes_its_value
                {
                    let reads_an_output_the_island_holds = function_nodes_each_after_what_it_reads
                        .get(*later_node_index)
                        .is_some_and(|later| *output_index < later.output_values.len());
                    if !reads_an_output_the_island_holds {
                        return Err(format!(
                            "function node `{}` reads output {output_index} of node \
                             {later_node_index}, which the island does not hold",
                            function_node.node_id
                        ));
                    }
                }
            }
        }
        let reads_an_output_of_the_island = function_nodes_each_after_what_it_reads
            .get(island_output_node_index)
            .is_some_and(|node| island_output_index < node.output_values.len());
        if !reads_an_output_of_the_island {
            return Err(format!(
                "the island's output names output {island_output_index} of node \
                 {island_output_node_index}, which the island does not hold"
            ));
        }
        let first_function_node_it_runs_as_python = function_nodes_each_after_what_it_reads
            .iter()
            .find(|function_node| function_node.how_it_is_called.runs_as_python())
            .map(|function_node| function_node.node_id.clone());
        let mut reader_input_ports_its_links_from_outside_it_arrive_at = Vec::new();
        for function_node in &function_nodes_each_after_what_it_reads {
            for where_it_takes_its_value in &function_node.where_each_input_takes_its_value {
                if let WhereAFunctionNodeInputTakesItsValue::ALinkFromOutsideTheIsland {
                    reader_input_port,
                    ..
                } = where_it_takes_its_value
                    && !reader_input_ports_its_links_from_outside_it_arrive_at
                        .contains(reader_input_port)
                {
                    reader_input_ports_its_links_from_outside_it_arrive_at
                        .push(reader_input_port.clone());
                }
            }
        }
        Ok(Self {
            island_output_node_index,
            island_output_index,
            first_function_node_it_runs_as_python,
            reader_input_ports_its_links_from_outside_it_arrive_at,
            evaluation: Mutex::new(FunctionNodeIslandEvaluation {
                function_nodes_each_after_what_it_reads,
                last_evaluated_at_ns: None,
                message_handed_on_since_the_last_evaluation: false,
                read_the_newest_message_at,
                values_last_read_off_links_from_outside_islands,
                claim_the_pooled_gpu_frame_a_message_names,
                call_breadcrumb_board_writer,
            }),
            stream_clock_now_ns,
        })
    }
}

/// The function node a helper runs as its own processor, once it is promoted
/// out of its readers' islands: called with the values its process reads.
#[pyclass(name = "FunctionNodeEvaluatedAsItsHelpersProcessor", frozen)]
pub(crate) struct FunctionNodeEvaluatedAsItsHelpersProcessor {
    function_node: Mutex<FunctionNodeInAnIsland>,
    call_breadcrumb_board_writer: Option<Arc<HelperProcessLossCountBoardWriter>>,
}

#[pymethods]
impl FunctionNodeEvaluatedAsItsHelpersProcessor {
    #[new]
    #[pyo3(signature = (
        *,
        link_data_access,
        node_id,
        called_through,
        input_count,
        config_values,
        config_value_indices_set_by_the_inputs_past_its_own,
        initial_state_values,
        output_count,
        outputs_that_may_hand_out_nothing = Vec::new(),
        inputs_carrying_an_unread_message = Vec::new(),
        outputs_carrying_an_unread_message = Vec::new(),
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        link_data_access: &PythonProcessorLinkDataAccess,
        node_id: String,
        called_through: Py<PyAny>,
        input_count: usize,
        config_values: Vec<f64>,
        config_value_indices_set_by_the_inputs_past_its_own: Vec<usize>,
        initial_state_values: Vec<f64>,
        output_count: usize,
        outputs_that_may_hand_out_nothing: Vec<usize>,
        inputs_carrying_an_unread_message: Vec<usize>,
        outputs_carrying_an_unread_message: Vec<usize>,
    ) -> PyResult<Self> {
        let breadcrumb_number =
            BREADCRUMB_NUMBER_OF_THE_FUNCTION_NODE_A_HELPER_RUNS_AS_ITS_PROCESSOR;
        let call_breadcrumb_board_writer =
            link_data_access.shared_loss_count_board_writer_of_this_helper();
        let writes_its_state_on_the_board = !initial_state_values.is_empty()
            && call_breadcrumb_board_writer
                .as_deref()
                .is_some_and(|board_writer| {
                    board_writer.carries_a_state_entry_for(breadcrumb_number)
                });
        let state_resumed_from = link_data_access
            .the_state_a_function_node_resumes_from(breadcrumb_number)
            .filter(|resumed| resumed.len() == initial_state_values.len());
        let resumes_from_the_state_its_stream_carries =
            state_resumed_from.is_some() && !initial_state_values.is_empty();
        let how_it_is_called = HowAFunctionNodeIsCalled::handed_over_by_the_bootstrap(
            &node_id,
            called_through,
            config_values,
            config_value_indices_set_by_the_inputs_past_its_own,
            input_count,
            the_state_a_function_node_starts_from(initial_state_values, state_resumed_from),
            resumes_from_the_state_its_stream_carries,
            output_count,
            &HowAFunctionNodeCarriesValuesBeyondNumbers {
                outputs_that_may_hand_out_nothing,
                inputs_carrying_an_unread_message,
                outputs_carrying_an_unread_message,
                ..HowAFunctionNodeCarriesValuesBeyondNumbers::default()
            },
        )
        .map_err(PyValueError::new_err)?;
        Ok(Self {
            function_node: Mutex::new(FunctionNodeInAnIsland {
                node_id,
                breadcrumb_number,
                how_it_is_called,
                where_each_input_takes_its_value: Vec::new(),
                inputs_that_take_none: Vec::new(),
                output_values: (0..output_count)
                    .map(|_| FunctionNodeValue::Number(0.0))
                    .collect(),
                writes_its_state_on_the_board,
            }),
            call_breadcrumb_board_writer,
        })
    }

    /// Call the node with `input_values` at the stream's time now, answering
    /// its outputs, a `float` for a number or bool, a `dict` as given, the
    /// `UnreadMessage` it hands on, and `None` for one that hands out nothing;
    /// refused when it refuses, keeping the state it had.
    fn evaluate<'py>(
        &self,
        python: Python<'py>,
        input_values: Vec<Bound<'py, PyAny>>,
    ) -> PyResult<Vec<Bound<'py, PyAny>>> {
        let now_ns = helper_stream_clock_now_ns();
        let input_values = input_values
            .iter()
            .map(FunctionNodeValue::from_python)
            .collect::<PyResult<Vec<_>>>()?;
        let function_node = &self.function_node;
        let call_breadcrumb_board_writer = self.call_breadcrumb_board_writer.as_deref();
        python.detach(|| {
            let mut function_node = function_node.lock();
            let function_node = &mut *function_node;
            if input_values.len() != function_node.how_it_is_called.input_count() {
                return Err(PyValueError::new_err(format!(
                    "function node `{}` takes {} inputs, and was called with {}",
                    function_node.node_id,
                    function_node.how_it_is_called.input_count(),
                    input_values.len()
                )));
            }
            let input_value_references: Vec<&FunctionNodeValue> = input_values.iter().collect();
            call_noting_its_breadcrumb_on(
                call_breadcrumb_board_writer,
                &function_node.node_id,
                function_node.breadcrumb_number,
                &mut function_node.how_it_is_called,
                &mut function_node.output_values,
                &input_value_references,
                now_ns,
            )
            .map_err(|refusal| PyValueError::new_err(refusal.to_string()))?;
            if let Some(board_writer) = call_breadcrumb_board_writer
                && function_node.writes_its_state_on_the_board
            {
                board_writer
                    .write_the_state_of_a_function_node(
                        function_node.breadcrumb_number,
                        function_node.how_it_is_called.state_values(),
                    )
                    .map_err(|refusal| PyValueError::new_err(refusal.to_string()))?;
            }
            Ok(())
        })?;
        let function_node = self.function_node.lock();
        function_node
            .output_values
            .iter()
            .map(|output_value| output_value.to_python(python))
            .collect()
    }
}

/// Feed `input_port` from the island of `function_nodes`, each after what it
/// reads, evaluated at each read of the port; an island already feeding the
/// port is replaced, and each stateful node it shares resumes from its state.
#[pyfunction]
#[pyo3(signature = (
    *,
    link_data_access,
    input_port,
    function_nodes,
    island_output,
    how_each_node_carries_its_values = Vec::new(),
))]
pub(crate) fn feed_an_input_port_from_a_function_node_island(
    link_data_access: &PythonProcessorLinkDataAccess,
    input_port: &str,
    function_nodes: Vec<FunctionNodeHandedOverByTheBootstrap>,
    island_output: (usize, usize),
    how_each_node_carries_its_values: Vec<HowAFunctionNodeCarriesValuesBeyondNumbers>,
) -> PyResult<()> {
    let input_mailboxes = link_data_access
        .input_mailboxes_of_this_helper()
        .ok_or_else(|| {
            PyValueError::new_err(
                "a function node island feeds only a helper process's own data plane",
            )
        })?;
    let states_the_island_it_replaces_last_returned: HashMap<u32, Vec<f64>> = input_mailboxes
        .the_state_each_stateful_function_node_of_the_island_feeding_last_returned(input_port)
        .into_iter()
        .collect();
    let island =
        FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
            function_nodes,
            island_output,
            Box::new(helper_stream_clock_now_ns),
            link_data_access.shared_loss_count_board_writer_of_this_helper(),
            &|breadcrumb_number| {
                states_the_island_it_replaces_last_returned
                    .get(&breadcrumb_number)
                    .cloned()
                    .or_else(|| {
                        link_data_access.the_state_a_function_node_resumes_from(breadcrumb_number)
                    })
            },
            HowAFunctionNodeIslandCarriesValuesBeyondNumbers {
                how_each_node_carries_its_values,
                read_the_newest_message_at: {
                    let input_mailboxes_read = Arc::downgrade(&input_mailboxes);
                    Box::new(
                        move |reader_input_port| match input_mailboxes_read.upgrade() {
                            Some(input_mailboxes) => {
                                the_newest_message_read_from(&input_mailboxes, reader_input_port)
                            }
                            None => Ok(None),
                        },
                    )
                },
                values_last_read_off_links_from_outside_islands: link_data_access
                    .values_last_read_off_links_into_its_function_node_islands(),
                claim_the_pooled_gpu_frame_a_message_names: link_data_access
                    .claim_of_the_pooled_gpu_frames_its_islands_hold(),
            },
        )
        .map_err(PyValueError::new_err)?;
    if let Some(function_node_id) = island.the_first_function_node_it_runs_as_python()
        && link_data_access.input_port_is_lent_to_native_code(input_port)
    {
        return Err(PyValueError::new_err(format!(
            "function node `{function_node_id}` cannot feed input port {input_port:?}: it runs \
             as Python, since its body does not compile, and the port is lent to native code, \
             whose read holds no Python interpreter lock to call it with. To fix: make \
             `{function_node_id}` compile, or read the port from Python"
        )));
    }
    input_mailboxes.feed_an_input_port_from_a_function_node_island(input_port, Arc::new(island));
    Ok(())
}

/// Stop feeding `input_port` from the island feeding it; whether one did.
#[pyfunction]
#[pyo3(signature = (*, link_data_access, input_port))]
pub(crate) fn stop_feeding_an_input_port_from_a_function_node_island(
    link_data_access: &PythonProcessorLinkDataAccess,
    input_port: &str,
) -> PyResult<bool> {
    let input_mailboxes = link_data_access
        .input_mailboxes_of_this_helper()
        .ok_or_else(|| {
            PyValueError::new_err(
                "a function node island feeds only a helper process's own data plane",
            )
        })?;
    Ok(input_mailboxes.stop_feeding_an_input_port_from_a_function_node_island(input_port))
}

/// The message `input_port` holds for this read, handed back unread: an
/// island's output at the port, or the newest message its link carried;
/// `None` when it holds none.
#[pyfunction]
#[pyo3(signature = (*, link_data_access, input_port))]
pub(crate) fn read_the_message_at_an_input_port_unread(
    python: Python<'_>,
    link_data_access: &PythonProcessorLinkDataAccess,
    input_port: &str,
) -> PyResult<Option<UnreadMessageAFunctionNodeHandsOn>> {
    Ok(link_data_access
        .read_the_message_at_an_input_port_as_it_arrived(python, input_port)?
        .map(|message| UnreadMessageAFunctionNodeHandsOn {
            message: Arc::new(
                MessageAFunctionNodeIslandHolds::hold_a_message_read_off_its_channel(
                    &message,
                    &link_data_access.claim_of_the_pooled_gpu_frames_its_islands_hold(),
                    input_port,
                ),
            ),
        }))
}

/// The number `input_port` holds for this read, read as an island reads one:
/// the first sample of a Channels block, f32 or f64, and any other message
/// refused by name; `None` when no message arrived since the last read.
#[pyfunction]
#[pyo3(signature = (*, link_data_access, node_id, input_port))]
pub(crate) fn read_the_control_value_at_an_input_port(
    python: Python<'_>,
    link_data_access: &PythonProcessorLinkDataAccess,
    node_id: &str,
    input_port: &str,
) -> PyResult<Option<f64>> {
    link_data_access
        .read_the_message_at_an_input_port_as_it_arrived(python, input_port)?
        .map(|message| the_control_value_a_message_carries(&message, node_id, input_port))
        .transpose()
        .map_err(|refusal| PyValueError::new_err(refusal.to_string()))
}

/// Write `message` on `output_port` exactly as it arrived, its descriptor,
/// stamp and group boundary unchanged.
#[pyfunction]
#[pyo3(signature = (*, link_data_access, output_port, message))]
pub(crate) fn hand_an_unread_message_on(
    python: Python<'_>,
    link_data_access: &PythonProcessorLinkDataAccess,
    output_port: &str,
    message: &UnreadMessageAFunctionNodeHandsOn,
) -> PyResult<()> {
    link_data_access.hand_a_message_on_unread(python, output_port, &message.message.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};

    /// The handover of a compiled entry: its address, as the bootstrap passes
    /// it, a Python int.
    fn compiled_to(evaluate_entry: ModuleHostTableFunctionNodeEvaluateEntry) -> Py<PyAny> {
        Python::initialize();
        Python::attach(|python| {
            (evaluate_entry as *const () as usize)
                .into_pyobject(python)
                .unwrap()
                .into_any()
                .unbind()
        })
    }

    fn a_stream_clock_the_test_sets() -> (Arc<AtomicI64>, Box<dyn Fn() -> i64 + Send + Sync>) {
        let stream_time_ns = Arc::new(AtomicI64::new(0));
        let read_by_the_island = Arc::clone(&stream_time_ns);
        (
            stream_time_ns,
            Box::new(move || read_by_the_island.load(Ordering::SeqCst)),
        )
    }

    unsafe extern "C" fn add_one_and_count_calls(
        input_values: *const f64,
        _config_values: *const f64,
        state_values: *mut f64,
        output_values: *mut f64,
        _now_ns: i64,
    ) -> i32 {
        unsafe {
            *state_values += 1.0;
            *output_values = *input_values + 1.0;
        }
        MODULE_HOST_TABLE_STATUS_OK
    }

    unsafe extern "C" fn scale_by_config_plus_seconds(
        input_values: *const f64,
        config_values: *const f64,
        _state_values: *mut f64,
        output_values: *mut f64,
        now_ns: i64,
    ) -> i32 {
        unsafe {
            *output_values = *input_values * *config_values + now_ns as f64 * 1e-9;
        }
        MODULE_HOST_TABLE_STATUS_OK
    }

    unsafe extern "C" fn refuse_after_writing_state(
        _input_values: *const f64,
        _config_values: *const f64,
        state_values: *mut f64,
        _output_values: *mut f64,
        _now_ns: i64,
    ) -> i32 {
        unsafe {
            *state_values = 99.0;
        }
        tatolab_module_sdk::module_host_table_abi::MODULE_HOST_TABLE_STATUS_REFUSED
    }

    fn an_island_of_add_one_then_scale(
        stream_clock_now_ns: Box<dyn Fn() -> i64 + Send + Sync>,
    ) -> FunctionNodeIslandEvaluatedInThisHelper {
        FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
            vec![
                (
                    "add_one".to_string(),
                    0,
                    compiled_to(add_one_and_count_calls),
                    vec![(None, 2.0)],
                    vec![],
                    vec![0.0],
                    1,
                    vec![],
                ),
                (
                    "scale".to_string(),
                    1,
                    compiled_to(scale_by_config_plus_seconds),
                    vec![(Some((0, 0)), 0.0)],
                    vec![10.0],
                    vec![],
                    1,
                    vec![],
                ),
            ],
            (1, 0),
            stream_clock_now_ns,
            None,
            &|_| None,
            HowAFunctionNodeIslandCarriesValuesBeyondNumbers::none(),
        )
        .unwrap()
    }

    #[test]
    fn each_node_reads_the_one_before_it_and_its_pin_and_config() {
        let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
        stream_time_ns.store(1_000_000_000, Ordering::SeqCst);
        let island = an_island_of_add_one_then_scale(stream_clock_now_ns);
        let output = island.evaluate_the_island_output_for_this_read().unwrap();
        assert_eq!(
            output.value,
            FunctionNodeIslandOutputValue::ControlValue((2.0 + 1.0) * 10.0 + 1.0)
        );
        assert_eq!(output.evaluated_at_ns, 1_000_000_000);
    }

    #[test]
    fn an_input_past_a_nodes_own_sets_the_config_value_it_names_at_each_evaluation() {
        let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
        stream_time_ns.store(0, Ordering::SeqCst);
        let island =
            FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
                vec![
                    (
                        "add_one".to_string(),
                        0,
                        compiled_to(add_one_and_count_calls),
                        vec![(None, 2.0)],
                        vec![],
                        vec![0.0],
                        1,
                        vec![],
                    ),
                    (
                        "scale".to_string(),
                        1,
                        compiled_to(scale_by_config_plus_seconds),
                        vec![(None, 4.0), (Some((0, 0)), 0.0)],
                        vec![10.0],
                        vec![],
                        1,
                        vec![0],
                    ),
                ],
                (1, 0),
                stream_clock_now_ns,
                None,
                &|_| None,
                HowAFunctionNodeIslandCarriesValuesBeyondNumbers::none(),
            )
            .unwrap();
        assert_eq!(
            island
                .evaluate_the_island_output_for_this_read()
                .unwrap()
                .value,
            FunctionNodeIslandOutputValue::ControlValue(4.0 * (2.0 + 1.0)),
            "the linked config value, not the one the node was handed, scales"
        );
    }

    #[test]
    fn a_config_value_an_input_sets_that_the_node_does_not_take_is_refused_naming_it() {
        let refusal =
            FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
                vec![(
                    "scale".to_string(),
                    0,
                    compiled_to(scale_by_config_plus_seconds),
                    vec![(None, 4.0), (None, 3.0)],
                    vec![10.0],
                    vec![],
                    1,
                    vec![1],
                )],
                (0, 0),
                a_stream_clock_the_test_sets().1,
                None,
                &|_| None,
                HowAFunctionNodeIslandCarriesValuesBeyondNumbers::none(),
            )
            .err()
            .unwrap();
        assert!(
            refusal.contains("`scale`") && refusal.contains("config value 1"),
            "got {refusal}"
        );
    }

    #[test]
    fn an_island_is_evaluated_once_per_stream_time() {
        let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
        stream_time_ns.store(5, Ordering::SeqCst);
        let island = an_island_of_add_one_then_scale(stream_clock_now_ns);
        island.evaluate_the_island_output_for_this_read().unwrap();
        island.evaluate_the_island_output_for_this_read().unwrap();
        let calls_at_one_time = island
            .evaluation
            .lock()
            .function_nodes_each_after_what_it_reads[0]
            .how_it_is_called
            .state_values()[0];
        assert_eq!(calls_at_one_time, 1.0);
        stream_time_ns.store(6, Ordering::SeqCst);
        island.evaluate_the_island_output_for_this_read().unwrap();
        assert_eq!(
            island
                .evaluation
                .lock()
                .function_nodes_each_after_what_it_reads[0]
                .how_it_is_called
                .state_values()[0],
            2.0
        );
    }

    #[test]
    fn a_refused_evaluation_keeps_the_state_it_was_called_with_and_names_the_node() {
        let island =
            FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
                vec![(
                    "refuses".to_string(),
                    2,
                    compiled_to(refuse_after_writing_state),
                    vec![],
                    vec![],
                    vec![1.0],
                    1,
                    vec![],
                )],
                (0, 0),
                a_stream_clock_the_test_sets().1,
                None,
                &|_| None,
                HowAFunctionNodeIslandCarriesValuesBeyondNumbers::none(),
            )
            .unwrap();
        let refusal = island
            .evaluate_the_island_output_for_this_read()
            .unwrap_err();
        assert!(refusal.to_string().contains("`refuses`"), "{refusal}");
        assert_eq!(
            island
                .evaluation
                .lock()
                .function_nodes_each_after_what_it_reads[0]
                .how_it_is_called
                .state_values(),
            [1.0]
        );
    }

    /// A board as a parent creates it for a helper calling the stateful node
    /// numbered `breadcrumb_number`, and the writer that helper opens on it.
    fn a_board_carrying_the_state_of_node(
        breadcrumb_number: u32,
    ) -> (
        streamlib::sdk::iceoryx2::HelperProcessLossCountBoard,
        Arc<HelperProcessLossCountBoardWriter>,
    ) {
        let board_name = format!(
            "island{}_state{breadcrumb_number}/loss-counts",
            std::process::id()
        );
        let parent_board = streamlib::sdk::iceoryx2::Iceoryx2Node::for_this_test_process()
            .create_helper_process_loss_count_board(
                &board_name,
                Vec::new(),
                vec![breadcrumb_number],
            )
            .unwrap();
        let board_writer = streamlib::sdk::iceoryx2::Iceoryx2Node::for_this_test_process()
            .open_helper_process_loss_count_board_writer(&board_name, &[], &[breadcrumb_number])
            .unwrap();
        (parent_board, Arc::new(board_writer))
    }

    #[test]
    fn a_stateful_node_resumes_from_the_state_its_stream_carries_and_writes_each_new_one() {
        let (parent_board, board_writer) = a_board_carrying_the_state_of_node(4);
        let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
        let island =
            FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
                vec![(
                    "counts_calls".to_string(),
                    4,
                    compiled_to(add_one_and_count_calls),
                    vec![(None, 0.0)],
                    vec![],
                    vec![0.0],
                    1,
                    vec![],
                )],
                (0, 0),
                stream_clock_now_ns,
                Some(board_writer),
                &|breadcrumb_number| (breadcrumb_number == 4).then(|| vec![41.0]),
                HowAFunctionNodeIslandCarriesValuesBeyondNumbers::none(),
            )
            .unwrap();
        assert_eq!(
            parent_board
                .function_node_state_entry(4)
                .unwrap()
                .state_values_written(),
            None,
            "nothing is written before the first call"
        );
        stream_time_ns.store(1, Ordering::SeqCst);
        island.evaluate_the_island_output_for_this_read().unwrap();
        assert_eq!(
            parent_board
                .function_node_state_entry(4)
                .unwrap()
                .state_values_written(),
            Some(&[42.0][..]),
            "the first call carries on from the state it resumed from"
        );
    }

    #[test]
    fn a_stored_state_of_another_length_is_not_resumed_from() {
        assert_eq!(
            the_state_a_function_node_starts_from(vec![0.0, 0.0], Some(vec![5.0])),
            vec![0.0, 0.0]
        );
        assert_eq!(
            the_state_a_function_node_starts_from(vec![0.0], Some(vec![5.0])),
            vec![5.0]
        );
        assert_eq!(
            the_state_a_function_node_starts_from(vec![1.0], None),
            vec![1.0]
        );
    }

    #[test]
    fn a_node_reading_one_not_evaluated_before_it_is_refused() {
        let refusal =
            FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
                vec![(
                    "reads_ahead".to_string(),
                    3,
                    compiled_to(add_one_and_count_calls),
                    vec![(Some((0, 0)), 0.0)],
                    vec![],
                    vec![0.0],
                    1,
                    vec![],
                )],
                (0, 0),
                a_stream_clock_the_test_sets().1,
                None,
                &|_| None,
                HowAFunctionNodeIslandCarriesValuesBeyondNumbers::none(),
            )
            .err()
            .unwrap();
        assert!(
            refusal.contains("not an output of a node evaluated before it"),
            "{refusal}"
        );
    }

    /// An object as the bootstrap builds one to run a body as Python: this one
    /// counts its calls in its state and answers a dict naming its input and
    /// the count, raising on a negative input.
    fn run_as_python_counting_its_calls() -> Py<PyAny> {
        Python::initialize();
        Python::attach(|python| {
            let namespace = PyDict::new(python);
            crate::python_class_from_source_for_tests::class_from_source_in_namespace(
                python,
                "class CountsItsCallsAsPython:\n\
                 \x20   def __init__(self):\n\
                 \x20       self.calls = 0.0\n\
                 \x20   def evaluate(self, input_values, now_ns):\n\
                 \x20       (x,) = input_values\n\
                 \x20       if x < 0:\n\
                 \x20           raise ValueError('a negative input')\n\
                 \x20       self.calls += 1.0\n\
                 \x20       return [{'x': x, 'calls': self.calls}, self.calls]\n\
                 \x20   def state_values(self):\n\
                 \x20       return [self.calls]\n\
                 \x20   def resume_from_state_values(self, state_values):\n\
                 \x20       (self.calls,) = state_values\n",
                "CountsItsCallsAsPython",
                &namespace,
            )
            .call0()
            .unwrap()
            .unbind()
        })
    }

    fn an_island_of_add_one_then_a_node_run_as_python(
        island_output: (usize, usize),
        stream_clock_now_ns: Box<dyn Fn() -> i64 + Send + Sync>,
        state_resumed_from: &dyn Fn(u32) -> Option<Vec<f64>>,
    ) -> FunctionNodeIslandEvaluatedInThisHelper {
        FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
            vec![
                (
                    "add_one".to_string(),
                    0,
                    compiled_to(add_one_and_count_calls),
                    vec![(None, 2.0)],
                    vec![],
                    vec![0.0],
                    1,
                    vec![],
                ),
                (
                    "tally".to_string(),
                    1,
                    run_as_python_counting_its_calls(),
                    vec![(Some((0, 0)), 0.0)],
                    vec![],
                    vec![0.0],
                    2,
                    vec![],
                ),
            ],
            island_output,
            stream_clock_now_ns,
            None,
            state_resumed_from,
            HowAFunctionNodeIslandCarriesValuesBeyondNumbers::none(),
        )
        .unwrap()
    }

    #[test]
    fn a_node_run_as_python_reads_a_compiled_nodes_output_and_gives_a_records_message() {
        let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
        stream_time_ns.store(7, Ordering::SeqCst);
        let island =
            an_island_of_add_one_then_a_node_run_as_python((1, 0), stream_clock_now_ns, &|_| None);
        assert_eq!(
            island.the_first_function_node_it_runs_as_python(),
            Some("tally")
        );
        let output = island.evaluate_the_island_output_for_this_read().unwrap();
        let FunctionNodeIslandOutputValue::RecordsMessage(records_message_payload) = output.value
        else {
            panic!("a dict output is a Records message: {:?}", output.value);
        };
        let record = rmpv::decode::read_value(&mut records_message_payload.as_slice()).unwrap();
        assert_eq!(
            record,
            rmpv::Value::Map(vec![
                (rmpv::Value::from("x"), rmpv::Value::from(3.0)),
                (rmpv::Value::from("calls"), rmpv::Value::from(1.0)),
            ])
        );
        assert_eq!(output.evaluated_at_ns, 7);
    }

    #[test]
    fn a_number_a_node_run_as_python_gives_is_a_control_value() {
        let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
        let island = an_island_of_add_one_then_a_node_run_as_python(
            (1, 1),
            stream_clock_now_ns,
            &|breadcrumb_number| (breadcrumb_number == 1).then(|| vec![41.0]),
        );
        stream_time_ns.store(1, Ordering::SeqCst);
        assert_eq!(
            island
                .evaluate_the_island_output_for_this_read()
                .unwrap()
                .value,
            FunctionNodeIslandOutputValue::ControlValue(42.0),
            "the node resumed from the state its stream carries"
        );
        assert_eq!(
            island
                .evaluation
                .lock()
                .function_nodes_each_after_what_it_reads[1]
                .how_it_is_called
                .state_values(),
            [42.0],
            "the state a node run as Python answers is what the board is written"
        );
    }

    #[test]
    fn an_island_of_compiled_nodes_runs_no_node_as_python() {
        let island = an_island_of_add_one_then_scale(a_stream_clock_the_test_sets().1);
        assert_eq!(island.the_first_function_node_it_runs_as_python(), None);
    }

    #[test]
    fn a_node_run_as_python_that_raises_is_refused_naming_it_and_what_it_raised() {
        let island =
            FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
                vec![(
                    "tally".to_string(),
                    5,
                    run_as_python_counting_its_calls(),
                    vec![(None, -1.0)],
                    vec![],
                    vec![0.0],
                    2,
                    vec![],
                )],
                (0, 1),
                a_stream_clock_the_test_sets().1,
                None,
                &|_| None,
                HowAFunctionNodeIslandCarriesValuesBeyondNumbers::none(),
            )
            .unwrap();
        let refusal = island
            .evaluate_the_island_output_for_this_read()
            .unwrap_err()
            .to_string();
        assert!(
            refusal.contains("`tally`, run as Python, raised"),
            "{refusal}"
        );
        assert!(refusal.contains("a negative input"), "{refusal}");
    }

    #[test]
    fn a_compiled_node_handed_a_dict_is_refused_naming_it() {
        let island =
            FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
                vec![
                    (
                        "tally".to_string(),
                        0,
                        run_as_python_counting_its_calls(),
                        vec![(None, 1.0)],
                        vec![],
                        vec![],
                        2,
                        vec![],
                    ),
                    (
                        "add_one".to_string(),
                        1,
                        compiled_to(add_one_and_count_calls),
                        vec![(Some((0, 0)), 0.0)],
                        vec![],
                        vec![0.0],
                        1,
                        vec![],
                    ),
                ],
                (1, 0),
                a_stream_clock_the_test_sets().1,
                None,
                &|_| None,
                HowAFunctionNodeIslandCarriesValuesBeyondNumbers::none(),
            )
            .unwrap();
        let refusal = island
            .evaluate_the_island_output_for_this_read()
            .unwrap_err()
            .to_string();
        assert!(
            refusal.contains("`add_one` runs compiled and takes a number on input 0"),
            "{refusal}"
        );
    }

    /// Hands its message input on while its second input is on, else nothing:
    /// a message travels as the token of the input holding it, and the slot
    /// past its one output says whether it handed one out.
    unsafe extern "C" fn hand_the_message_on_while_open(
        input_values: *const f64,
        _config_values: *const f64,
        _state_values: *mut f64,
        output_values: *mut f64,
        _now_ns: i64,
    ) -> i32 {
        unsafe {
            let is_open = *input_values.add(1) != 0.0;
            *output_values = if is_open { *input_values } else { 0.0 };
            *output_values.add(1) = if is_open { 1.0 } else { 0.0 };
        }
        MODULE_HOST_TABLE_STATUS_OK
    }

    const READER_INPUT_PORT_OF_THE_LINK_FROM_OUTSIDE_THE_ISLAND: &str =
        "function_node_island_input_link_from_a_class_node";

    /// Messages queued at the reader input port a link from outside the
    /// island is read at, each read handing out the newest.
    fn messages_queued_at_the_reader_input_port() -> (
        Arc<Mutex<Vec<MessageReadFromInputPort>>>,
        NewestMessageReaderOfTheReadersInputPorts,
    ) {
        let queued: Arc<Mutex<Vec<MessageReadFromInputPort>>> = Arc::new(Mutex::new(Vec::new()));
        let read_by_the_island = Arc::clone(&queued);
        (
            queued,
            Box::new(move |reader_input_port| {
                assert_eq!(
                    reader_input_port,
                    READER_INPUT_PORT_OF_THE_LINK_FROM_OUTSIDE_THE_ISLAND
                );
                let mut queued = read_by_the_island.lock();
                let newest = queued.pop();
                queued.clear();
                Ok(newest)
            }),
        )
    }

    fn a_message_of(
        payload: Vec<u8>,
        payload_descriptor: streamlib::sdk::iceoryx2::DataChannelPayloadDescriptor,
        timestamp_ns: i64,
    ) -> MessageReadFromInputPort {
        MessageReadFromInputPort {
            payload: payload.into(),
            timestamp_ns,
            group_boundary: streamlib::sdk::iceoryx2::MessageGroupBoundary::StartsGroup,
            lost_since_last_read: 2,
            payload_descriptor,
        }
    }

    fn a_channels_block_of_one_f64_sample(
        sample: f64,
        timestamp_ns: i64,
    ) -> MessageReadFromInputPort {
        a_message_of(
            sample.to_le_bytes().to_vec(),
            streamlib::sdk::iceoryx2::DataChannelPayloadDescriptor::channels_block(
                ChannelsBlockDescriptorBody {
                    start_sample: 0,
                    sample_rate: 0,
                    channel_count: 1,
                    samples_per_channel: 1,
                    sample_type: ChannelsBlockSampleType::F64 as u32,
                },
            ),
            timestamp_ns,
        )
    }

    #[test]
    fn a_control_value_is_the_first_sample_of_an_f32_or_f64_block_of_any_shape() {
        let samples_of_two_channels_of_three: [f32; 6] = [0.25, 0.5, 0.75, -1.0, -0.5, 0.0];
        let f32_block_of_two_channels_of_three = a_message_of(
            samples_of_two_channels_of_three
                .iter()
                .flat_map(|sample| sample.to_le_bytes())
                .collect(),
            streamlib::sdk::iceoryx2::DataChannelPayloadDescriptor::channels_block(
                ChannelsBlockDescriptorBody {
                    start_sample: 0,
                    sample_rate: 48_000,
                    channel_count: 2,
                    samples_per_channel: 3,
                    sample_type: ChannelsBlockSampleType::F32 as u32,
                },
            ),
            10,
        );
        assert_eq!(
            the_control_value_a_message_carries(&f32_block_of_two_channels_of_three, "n", "x")
                .unwrap(),
            0.25
        );
        assert_eq!(
            the_control_value_a_message_carries(
                &a_channels_block_of_one_f64_sample(7.5, 20),
                "n",
                "x"
            )
            .unwrap(),
            7.5
        );
    }

    #[test]
    fn a_control_value_read_off_a_message_that_is_no_channels_block_is_refused_by_name() {
        let records_message = a_message_of(
            vec![0x80],
            streamlib::sdk::iceoryx2::DataChannelPayloadDescriptor::records_message(),
            10,
        );
        let refusal = the_control_value_a_message_carries(&records_message, "scale-1", "x")
            .expect_err("a Records message carries no number");
        assert!(
            refusal
                .to_string()
                .contains("function node `scale-1` reads a number")
                && refusal.to_string().contains("is no Channels block"),
            "{refusal}"
        );
    }

    fn a_gate_reading_a_link_from_outside_its_island(
        is_open: bool,
        stream_clock_now_ns: Box<dyn Fn() -> i64 + Send + Sync>,
        read_the_newest_message_at: NewestMessageReaderOfTheReadersInputPorts,
        claim_the_pooled_gpu_frame_a_message_names: ClaimOfThePooledGpuFrameAMessageNames,
    ) -> FunctionNodeIslandEvaluatedInThisHelper {
        FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
            vec![(
                "gate".to_string(),
                0,
                compiled_to(hand_the_message_on_while_open),
                vec![(None, 0.0), (None, if is_open { 1.0 } else { 0.0 })],
                vec![],
                vec![],
                1,
                vec![],
            )],
            (0, 0),
            stream_clock_now_ns,
            None,
            &|_| None,
            HowAFunctionNodeIslandCarriesValuesBeyondNumbers {
                how_each_node_carries_its_values: vec![
                    HowAFunctionNodeCarriesValuesBeyondNumbers {
                        inputs_read_from_links_from_outside_the_island: vec![(
                            0,
                            READER_INPUT_PORT_OF_THE_LINK_FROM_OUTSIDE_THE_ISLAND.to_string(),
                            "unread_message".to_string(),
                        )],
                        inputs_that_take_none: vec![],
                        inputs_carrying_an_unread_message: vec![0],
                        outputs_carrying_an_unread_message: vec![0],
                        outputs_that_may_hand_out_nothing: vec![0],
                    },
                ],
                read_the_newest_message_at,
                values_last_read_off_links_from_outside_islands: Arc::default(),
                claim_the_pooled_gpu_frame_a_message_names,
            },
        )
        .unwrap()
    }

    #[test]
    fn an_open_gate_hands_on_the_newest_message_unread_and_nothing_once_it_is_handed_on() {
        let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
        let (queued, read_the_newest_message_at) = messages_queued_at_the_reader_input_port();
        let island = a_gate_reading_a_link_from_outside_its_island(
            true,
            stream_clock_now_ns,
            read_the_newest_message_at,
            Box::new(|_| Ok(None)),
        );
        queued.lock().push(a_message_of(
            vec![1, 2, 3],
            streamlib::sdk::iceoryx2::DataChannelPayloadDescriptor::bytes(),
            40,
        ));
        queued.lock().push(a_message_of(
            vec![4, 5],
            streamlib::sdk::iceoryx2::DataChannelPayloadDescriptor::bytes(),
            50,
        ));
        stream_time_ns.store(100, Ordering::SeqCst);
        let FunctionNodeIslandOutputValue::MessageHandedOnUnread(handed_on) = island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value
        else {
            panic!("an open gate hands its message on");
        };
        assert_eq!(handed_on.payload_bytes, vec![4, 5]);
        assert_eq!(handed_on.timestamp_ns, 50);
        assert_eq!(handed_on.lost_since_last_read, 2);
        assert_eq!(
            island
                .evaluate_the_island_output_for_this_read()
                .unwrap()
                .value,
            FunctionNodeIslandOutputValue::Nothing,
            "a second read at the same stream time is not handed the message again"
        );
        stream_time_ns.store(200, Ordering::SeqCst);
        assert_eq!(
            island
                .evaluate_the_island_output_for_this_read()
                .unwrap()
                .value,
            FunctionNodeIslandOutputValue::Nothing
        );
    }

    #[test]
    fn a_pooled_gpu_frame_a_message_names_is_claimed_while_the_island_holds_it_and_released_after()
    {
        let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
        let (queued, read_the_newest_message_at) = messages_queued_at_the_reader_input_port();
        let claims_held: Arc<Mutex<Vec<std::sync::Weak<u32>>>> = Arc::new(Mutex::new(Vec::new()));
        let claims_noted_by_the_island = Arc::clone(&claims_held);
        let island = a_gate_reading_a_link_from_outside_its_island(
            false,
            stream_clock_now_ns,
            read_the_newest_message_at,
            Box::new(move |pooled_gpu_frame: GpuPoolSlotFrameDescriptorBody| {
                let claim = Arc::new(pooled_gpu_frame.slot);
                claims_noted_by_the_island
                    .lock()
                    .push(Arc::downgrade(&claim));
                Ok(Some(Box::new(claim) as PooledGpuFrameClaimAnIslandHolds))
            }),
        );
        let pooled_gpu_frame = GpuPoolSlotFrameDescriptorBody {
            pool: [7; 16],
            slot: 3,
            generation: 1,
            timeline_value: 9,
            image_layout: 0,
        };
        queued.lock().push(a_message_of(
            vec![],
            DataChannelPayloadDescriptor::gpu_pool_slot_frame(pooled_gpu_frame),
            10,
        ));
        stream_time_ns.store(100, Ordering::SeqCst);
        island.evaluate_the_island_output_for_this_read().unwrap();
        {
            let claims_held = claims_held.lock();
            assert_eq!(claims_held.len(), 1);
            assert_eq!(claims_held[0].upgrade().as_deref(), Some(&3));
        }
        stream_time_ns.store(200, Ordering::SeqCst);
        island.evaluate_the_island_output_for_this_read().unwrap();
        let claims_held = claims_held.lock();
        assert_eq!(claims_held.len(), 1);
        assert!(claims_held[0].upgrade().is_none());
    }

    #[test]
    fn a_copy_of_an_unread_message_a_state_record_keeps_is_the_same_message() {
        Python::attach(|python| {
            let unread_message = Py::new(
                python,
                UnreadMessageAFunctionNodeHandsOn {
                    message: Arc::new(MessageAFunctionNodeIslandHolds {
                        message: MessageAFunctionNodeIslandHandsOnUnread::read_off_its_channel(
                            &a_message_of(vec![1], DataChannelPayloadDescriptor::bytes(), 10),
                        ),
                        _claim_of_its_pooled_gpu_frame: None,
                    }),
                },
            )
            .unwrap();
            let copy_module = python.import("copy").unwrap();
            for copy_function in ["copy", "deepcopy"] {
                let copied = copy_module
                    .call_method1(copy_function, (unread_message.clone_ref(python),))
                    .unwrap();
                assert!(copied.is(unread_message.bind(python)), "{copy_function}");
            }
        });
    }

    #[test]
    fn a_closed_gate_hands_out_nothing_rather_than_the_last_message() {
        let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
        let (queued, read_the_newest_message_at) = messages_queued_at_the_reader_input_port();
        let island = a_gate_reading_a_link_from_outside_its_island(
            false,
            stream_clock_now_ns,
            read_the_newest_message_at,
            Box::new(|_| Ok(None)),
        );
        assert_eq!(
            island.the_reader_input_ports_its_links_from_outside_it_arrive_at(),
            [READER_INPUT_PORT_OF_THE_LINK_FROM_OUTSIDE_THE_ISLAND.to_string()],
            "the island names the reader input port its link arrives at, so a message there \
             is judged by what the island hands out for it"
        );
        queued.lock().push(a_message_of(
            vec![1],
            streamlib::sdk::iceoryx2::DataChannelPayloadDescriptor::bytes(),
            10,
        ));
        stream_time_ns.store(100, Ordering::SeqCst);
        assert_eq!(
            island
                .evaluate_the_island_output_for_this_read()
                .unwrap()
                .value,
            FunctionNodeIslandOutputValue::Nothing
        );
    }

    #[test]
    fn a_number_read_off_a_link_from_outside_the_island_keeps_the_newest_control_value() {
        let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
        let (queued, read_the_newest_message_at) = messages_queued_at_the_reader_input_port();
        let island =
            FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
                vec![(
                    "add_one".to_string(),
                    0,
                    compiled_to(add_one_and_count_calls),
                    vec![(None, 0.0)],
                    vec![],
                    vec![0.0],
                    1,
                    vec![],
                )],
                (0, 0),
                stream_clock_now_ns,
                None,
                &|_| None,
                HowAFunctionNodeIslandCarriesValuesBeyondNumbers {
                    how_each_node_carries_its_values: vec![
                        HowAFunctionNodeCarriesValuesBeyondNumbers {
                            inputs_read_from_links_from_outside_the_island: vec![(
                                0,
                                READER_INPUT_PORT_OF_THE_LINK_FROM_OUTSIDE_THE_ISLAND.to_string(),
                                "float".to_string(),
                            )],
                            ..Default::default()
                        },
                    ],
                    read_the_newest_message_at,
                    values_last_read_off_links_from_outside_islands: Arc::default(),
                    claim_the_pooled_gpu_frame_a_message_names: Box::new(|_| Ok(None)),
                },
            )
            .unwrap();
        stream_time_ns.store(100, Ordering::SeqCst);
        assert_eq!(
            island
                .evaluate_the_island_output_for_this_read()
                .unwrap()
                .value,
            FunctionNodeIslandOutputValue::Nothing,
            "a node whose link has carried no value yet hands out nothing"
        );
        queued
            .lock()
            .push(a_channels_block_of_one_f64_sample(3.0, 10));
        queued
            .lock()
            .push(a_channels_block_of_one_f64_sample(5.0, 20));
        stream_time_ns.store(200, Ordering::SeqCst);
        assert_eq!(
            island
                .evaluate_the_island_output_for_this_read()
                .unwrap()
                .value,
            FunctionNodeIslandOutputValue::ControlValue(6.0)
        );
        stream_time_ns.store(300, Ordering::SeqCst);
        assert_eq!(
            island
                .evaluate_the_island_output_for_this_read()
                .unwrap()
                .value,
            FunctionNodeIslandOutputValue::ControlValue(6.0),
            "a number keeps the newest value its link carried"
        );
    }

    fn an_add_one_island_reading_a_link_from_outside_its_island(
        stream_clock_now_ns: Box<dyn Fn() -> i64 + Send + Sync>,
        read_the_newest_message_at: NewestMessageReaderOfTheReadersInputPorts,
        values_last_read_off_links_from_outside_islands: ValuesLastReadOffLinksFromOutsideIslands,
    ) -> FunctionNodeIslandEvaluatedInThisHelper {
        FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
            vec![(
                "add_one".to_string(),
                0,
                compiled_to(add_one_and_count_calls),
                vec![(None, 0.0)],
                vec![],
                vec![0.0],
                1,
                vec![],
            )],
            (0, 0),
            stream_clock_now_ns,
            None,
            &|_| None,
            HowAFunctionNodeIslandCarriesValuesBeyondNumbers {
                how_each_node_carries_its_values: vec![
                    HowAFunctionNodeCarriesValuesBeyondNumbers {
                        inputs_read_from_links_from_outside_the_island: vec![(
                            0,
                            READER_INPUT_PORT_OF_THE_LINK_FROM_OUTSIDE_THE_ISLAND.to_string(),
                            "float".to_string(),
                        )],
                        ..Default::default()
                    },
                ],
                read_the_newest_message_at,
                values_last_read_off_links_from_outside_islands,
                claim_the_pooled_gpu_frame_a_message_names: Box::new(|_| Ok(None)),
            },
        )
        .unwrap()
    }

    #[test]
    fn an_island_replacing_another_starts_from_the_control_value_its_link_last_carried() {
        let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
        let (queued, read_the_newest_message_at) = messages_queued_at_the_reader_input_port();
        let values_last_read: ValuesLastReadOffLinksFromOutsideIslands = Arc::default();
        let island = an_add_one_island_reading_a_link_from_outside_its_island(
            stream_clock_now_ns,
            read_the_newest_message_at,
            Arc::clone(&values_last_read),
        );
        queued
            .lock()
            .push(a_channels_block_of_one_f64_sample(5.0, 10));
        stream_time_ns.store(100, Ordering::SeqCst);
        assert_eq!(
            island
                .evaluate_the_island_output_for_this_read()
                .unwrap()
                .value,
            FunctionNodeIslandOutputValue::ControlValue(6.0)
        );

        let (replacement_stream_time_ns, replacement_stream_clock_now_ns) =
            a_stream_clock_the_test_sets();
        let (_, replacement_reads_the_newest_message_at) =
            messages_queued_at_the_reader_input_port();
        let replacement = an_add_one_island_reading_a_link_from_outside_its_island(
            replacement_stream_clock_now_ns,
            replacement_reads_the_newest_message_at,
            values_last_read,
        );
        replacement_stream_time_ns.store(200, Ordering::SeqCst);
        assert_eq!(
            replacement
                .evaluate_the_island_output_for_this_read()
                .unwrap()
                .value,
            FunctionNodeIslandOutputValue::ControlValue(6.0)
        );
    }

    /// Counts its calls in its state and hands out a token naming an input it
    /// does not have.
    unsafe extern "C" fn count_calls_and_hand_out_a_token_naming_no_message_input(
        _input_values: *const f64,
        _config_values: *const f64,
        state_values: *mut f64,
        output_values: *mut f64,
        _now_ns: i64,
    ) -> i32 {
        unsafe {
            *state_values += 1.0;
            *output_values = 7.0;
        }
        MODULE_HOST_TABLE_STATUS_OK
    }

    #[test]
    fn a_call_handing_out_a_token_naming_no_message_input_keeps_the_state_it_started_from() {
        let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
        let island =
            FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
                vec![(
                    "hand_on".to_string(),
                    0,
                    compiled_to(count_calls_and_hand_out_a_token_naming_no_message_input),
                    vec![(None, 0.0)],
                    vec![],
                    vec![0.0],
                    1,
                    vec![],
                )],
                (0, 0),
                stream_clock_now_ns,
                None,
                &|_| None,
                HowAFunctionNodeIslandCarriesValuesBeyondNumbers {
                    how_each_node_carries_its_values: vec![
                        HowAFunctionNodeCarriesValuesBeyondNumbers {
                            inputs_that_take_none: vec![0],
                            inputs_carrying_an_unread_message: vec![0],
                            outputs_carrying_an_unread_message: vec![0],
                            ..Default::default()
                        },
                    ],
                    read_the_newest_message_at: Box::new(|_| Ok(None)),
                    values_last_read_off_links_from_outside_islands: Arc::default(),
                    claim_the_pooled_gpu_frame_a_message_names: Box::new(|_| Ok(None)),
                },
            )
            .unwrap();
        stream_time_ns.store(100, Ordering::SeqCst);

        island
            .evaluate_the_island_output_for_this_read()
            .expect_err("a token naming no message input is refused");

        assert_eq!(
            island.the_state_each_stateful_function_node_last_returned(),
            vec![(0, vec![0.0])]
        );
    }
}
