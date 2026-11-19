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
use pyo3::types::{PyDict, PyTuple};
use streamlib::sdk::descriptors::{PinType, VectorComponentCount};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::{
    DataChannelPayloadDescriptor, DataChannelPayloadDescriptorKind,
    FunctionNodeIslandEvaluatedAtEachRead, FunctionNodeIslandOutputValue,
    GpuPoolSlotFrameDescriptorBody, HelperProcessLossCountBoardWriter, ImageFrameDescriptorBody,
    InputMailboxesInner, MessageAFunctionNodeIslandHandsOnUnread, MessageReadFromInputPort,
};

use streamlib::sdk::pin_value_packing::{
    PackedPinValueRef, the_channel_values_a_control_message_carries,
    the_channel_values_a_pin_type_reads,
};

use crate::python_bag_conversion::{records_message_dict_of, records_message_payload_of};
use crate::python_helper_stream_clock::helper_stream_clock_now_ns;
use crate::python_logging::monotonic_clock_now_ns;
use crate::python_node_link_data_access::PythonNodeLinkDataAccess;

mod function_node_call_shapes;
mod function_node_island_evaluators;
mod function_node_raises_in_this_helper;

use function_node_call_shapes::{FunctionNodeCallFailure, HowAFunctionNodeIsCalled};
use function_node_island_evaluators::FunctionNodeHandedOverByTheBootstrap;
pub(crate) use function_node_island_evaluators::{
    FunctionNodeEvaluatedAsAStandaloneNode, FunctionNodeIslandEvaluatedInThisHelper,
};
pub(crate) use function_node_raises_in_this_helper::FunctionNodeRaisesInThisHelper;
use function_node_raises_in_this_helper::WhereAnIslandNotesTheRaisesOfItsFunctionNodes;

/// One value a function node takes or gives.
#[derive(Debug)]
pub(crate) enum FunctionNodeValue {
    /// A number or a bool, as the f64 a compiled entry carries.
    Number(f64),
    /// A `dict`, which only a node run as Python takes or gives.
    Record(Py<PyAny>),
    /// A Color, a Vector, a Rectangle or a Transform: its parts in the order its stored form
    /// lists them, which only a node run as Python takes or gives.
    Parts(Vec<f64>),
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
            Self::Parts(parts) => Err(Error::Runtime(format!(
                "function node `{node_id}` runs compiled and takes a number on input \
                 {input_index}, and was handed a value of {} parts",
                parts.len()
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
            Self::Parts(parts) => Self::Parts(parts.clone()),
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
            Self::Parts(parts) => PyTuple::new(python, parts)?.into_any(),
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
    /// was handed as that message, a `dict` as a record, a `tuple` of numbers
    /// as the parts of a Color, a Vector, a Rectangle or a Transform, anything
    /// else as the number it converts to.
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
        if value.is_instance_of::<PyTuple>() {
            return Ok(Self::Parts(value.extract::<Vec<f64>>()?));
        }
        Ok(Self::Number(value.extract::<f64>()?))
    }

    fn as_the_island_output_of(
        &self,
        node_id: &str,
        output_index: usize,
    ) -> Result<FunctionNodeIslandOutputValue> {
        Ok(match self {
            Self::Number(number) => FunctionNodeIslandOutputValue::ControlValue(vec![*number]),
            Self::Parts(parts) => FunctionNodeIslandOutputValue::ControlValue(parts.clone()),
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
    fn __deepcopy__(
        slf: Py<Self>,
        #[allow(
            unused_variables,
            reason = "the deep copy shares the message, so it never reads the memo"
        )]
        memo: &Bound<'_, PyAny>,
    ) -> Py<Self> {
        slf
    }
}

/// How a function node input reads the messages a link from outside its
/// island carries.
#[derive(Debug, Clone, PartialEq)]
enum HowAnInputReadsALinkFromOutsideItsIsland {
    /// As the number a control message carries: the first sample of its first
    /// channel.
    ControlValue,
    /// As the parts of a value of the pin type, one on each channel of a
    /// control message, read with the free conversion a link allows.
    Parts(PinType),
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
            value_type => the_pin_type_a_value_type_carries_in_parts(value_type)
                .map(Self::Parts)
                .ok_or_else(|| {
                    format!(
                        "an input of value type {value_type:?} cannot read a link from a node \
                         outside its island; it reads a number, a Color, a Vector, a \
                         Rectangle, a Transform, a dict or an `UnreadMessage`"
                    )
                }),
        }
    }
}

/// The pin type of a value the bootstrap names `value_type` and carries as its
/// parts: a Color, a Vector of each size, a Rectangle or a Transform.
///
/// Wire contract with the helper bootstrap: these value type names.
pub(crate) fn the_pin_type_a_value_type_carries_in_parts(value_type: &str) -> Option<PinType> {
    Some(match value_type {
        "color" => PinType::Color {},
        "vector2" => PinType::Vector {
            component_count: VectorComponentCount::Two,
        },
        "vector3" => PinType::Vector {
            component_count: VectorComponentCount::Three,
        },
        "vector4" => PinType::Vector {
            component_count: VectorComponentCount::Four,
        },
        "rectangle" => PinType::Rectangle {},
        "transform" => PinType::Transform {},
        _ => return None,
    })
}

/// The parts a control message carries, read as `read_as` reads them: one
/// number, the first sample of its first channel, for `None`; the parts of a
/// value of the pin type for `Some`, with the free conversion a link allows.
fn the_parts_a_message_carries(
    message: &MessageReadFromInputPort,
    read_as: Option<&PinType>,
    node_id: &str,
    reader_input_port: &str,
) -> Result<Vec<f64>> {
    let channel_values = the_channel_values_a_control_message_carries(PackedPinValueRef {
        payload_descriptor: &message.payload_descriptor,
        payload: message.payload.bytes(),
    });
    let read = match (channel_values, read_as) {
        (Ok(channel_values), None) => Ok(channel_values.into_iter().take(1).collect()),
        (Ok(channel_values), Some(pin_type)) => {
            the_channel_values_a_pin_type_reads(pin_type, &channel_values)
        }
        (Err(refusal), _) => Err(refusal),
    };
    read.map_err(|refusal| {
        Error::Runtime(format!(
            "function node `{node_id}` reads {} off the link it reads at \
             {reader_input_port:?}, and {refusal}",
            read_as.map_or_else(|| "a number".to_owned(), |pin_type| format!("a {pin_type}"))
        ))
    })
}

/// The number a control message carries: the first sample of its first channel.
fn the_control_value_a_message_carries(
    message: &MessageReadFromInputPort,
    node_id: &str,
    reader_input_port: &str,
) -> Result<f64> {
    Ok(the_parts_a_message_carries(message, None, node_id, reader_input_port)?[0])
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
    where_it_notes_the_raises_of_its_function_nodes: WhereAnIslandNotesTheRaisesOfItsFunctionNodes,
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
                match (newest_message, &*read_as) {
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
                    (Some(message), HowAnInputReadsALinkFromOutsideItsIsland::Parts(pin_type)) => {
                        *value_read_last = FunctionNodeValue::Parts(the_parts_a_message_carries(
                            &message,
                            Some(pin_type),
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

    /// Call each node in turn at `now_ns`. A node whose call fails hands out
    /// nothing, its raise noted, so each node reading it takes nothing.
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
                hand_out_nothing_on_every_output(&mut function_node.output_values);
                continue;
            }
            let called = call_noting_its_breadcrumb_on(
                self.call_breadcrumb_board_writer.as_deref(),
                &function_node.node_id,
                function_node.breadcrumb_number,
                &mut function_node.how_it_is_called,
                &mut function_node.output_values,
                &input_values,
                now_ns,
            );
            if let Err(call_failure) = called {
                self.where_it_notes_the_raises_of_its_function_nodes
                    .note_that_it_raised(&function_node.node_id, || call_failure.described());
                hand_out_nothing_on_every_output(&mut function_node.output_values);
                continue;
            }
            self.where_it_notes_the_raises_of_its_function_nodes
                .note_that_it_ran_cleanly(&function_node.node_id);
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

/// Set every output of a node that answered nothing this evaluation to nothing.
fn hand_out_nothing_on_every_output(output_values: &mut [FunctionNodeValue]) {
    for output_value in output_values {
        *output_value = FunctionNodeValue::Nothing;
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
) -> std::result::Result<(), FunctionNodeCallFailure> {
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
    link_data_access: &PythonNodeLinkDataAccess,
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
            WhereAnIslandNotesTheRaisesOfItsFunctionNodes {
                function_node_raises_in_this_helper: link_data_access
                    .function_node_raises_in_this_helper(),
                island_input_port: input_port.to_string(),
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
    link_data_access
        .function_node_raises_in_this_helper()
        .forget_the_island_feeding(input_port);
    input_mailboxes.feed_an_input_port_from_a_function_node_island(input_port, Arc::new(island));
    Ok(())
}

/// Stop feeding `input_port` from the island feeding it; whether one did.
#[pyfunction]
#[pyo3(signature = (*, link_data_access, input_port))]
pub(crate) fn stop_feeding_an_input_port_from_a_function_node_island(
    link_data_access: &PythonNodeLinkDataAccess,
    input_port: &str,
) -> PyResult<bool> {
    let input_mailboxes = link_data_access
        .input_mailboxes_of_this_helper()
        .ok_or_else(|| {
            PyValueError::new_err(
                "a function node island feeds only a helper process's own data plane",
            )
        })?;
    link_data_access
        .function_node_raises_in_this_helper()
        .forget_the_island_feeding(input_port);
    Ok(input_mailboxes.stop_feeding_an_input_port_from_a_function_node_island(input_port))
}

/// What each function node this helper's islands call raised at its last call,
/// by the node's id; a node that ran cleanly is left out.
#[pyfunction]
#[pyo3(signature = (*, link_data_access))]
pub(crate) fn why_each_function_node_its_islands_call_is_raising(
    link_data_access: &PythonNodeLinkDataAccess,
) -> std::collections::BTreeMap<String, String> {
    link_data_access
        .function_node_raises_in_this_helper()
        .why_each_function_node_is_raising()
}

/// Each first raise of a run of a function node's raises noted since the last
/// call, in the order noted: the node's id, what it raised as the graph shows
/// it, and its Python traceback, `None` for a call refused outside Python.
#[pyfunction]
#[pyo3(signature = (*, link_data_access))]
pub(crate) fn take_each_function_node_raise_newly_noted(
    link_data_access: &PythonNodeLinkDataAccess,
) -> Vec<(String, String, Option<String>)> {
    link_data_access
        .function_node_raises_in_this_helper()
        .take_each_raise_newly_noted()
        .into_iter()
        .map(|newly_noted| {
            (
                newly_noted.function_node_id,
                newly_noted.raise.raising_because,
                newly_noted.raise.traceback,
            )
        })
        .collect()
}

/// The message `input_port` holds for this read, handed back unread: an
/// island's output at the port, or the newest message its link carried;
/// `None` when it holds none.
#[pyfunction]
#[pyo3(signature = (*, link_data_access, input_port))]
pub(crate) fn read_the_message_at_an_input_port_unread(
    python: Python<'_>,
    link_data_access: &PythonNodeLinkDataAccess,
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
    link_data_access: &PythonNodeLinkDataAccess,
    node_id: &str,
    input_port: &str,
) -> PyResult<Option<f64>> {
    link_data_access
        .read_the_message_at_an_input_port_as_it_arrived(python, input_port)?
        .map(|message| the_control_value_a_message_carries(&message, node_id, input_port))
        .transpose()
        .map_err(|refusal| PyValueError::new_err(refusal.to_string()))
}

/// The parts of the value `input_port` holds for this read, a value of the pin
/// type the bootstrap names `value_type`, read as an island reads one off a
/// control message with the free conversion a link allows, and any other
/// message refused by name; `None` when no message arrived since the last read.
#[pyfunction]
#[pyo3(signature = (*, link_data_access, node_id, input_port, value_type))]
pub(crate) fn read_the_parts_at_an_input_port<'py>(
    python: Python<'py>,
    link_data_access: &PythonNodeLinkDataAccess,
    node_id: &str,
    input_port: &str,
    value_type: &str,
) -> PyResult<Option<Bound<'py, PyTuple>>> {
    let pin_type = the_pin_type_a_value_type_carries_in_parts(value_type).ok_or_else(|| {
        PyValueError::new_err(format!(
            "value type {value_type:?} is no Color, Vector, Rectangle or Transform"
        ))
    })?;
    link_data_access
        .read_the_message_at_an_input_port_as_it_arrived(python, input_port)?
        .map(|message| {
            the_parts_a_message_carries(&message, Some(&pin_type), node_id, input_port)
                .map_err(|refusal| PyValueError::new_err(refusal.to_string()))
                .and_then(|parts| PyTuple::new(python, parts))
        })
        .transpose()
}

/// Write `message` on `output_port` exactly as it arrived, its descriptor,
/// stamp and group boundary unchanged.
#[pyfunction]
#[pyo3(signature = (*, link_data_access, output_port, message))]
pub(crate) fn hand_an_unread_message_on(
    python: Python<'_>,
    link_data_access: &PythonNodeLinkDataAccess,
    output_port: &str,
    message: &UnreadMessageAFunctionNodeHandsOn,
) -> PyResult<()> {
    link_data_access.hand_a_message_on_unread(python, output_port, &message.message.message)
}

#[cfg(test)]
mod tests;
