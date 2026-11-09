// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! An input port fed by a function node island: no channel and no mailbox, the
//! island evaluated each time the port is read.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::core::error::Result;
use crate::iceoryx2::{InputPortMessagePayload, MessageGroupBoundary, MessageReadFromInputPort};
use streamlib_ipc_types::{
    ChannelsBlockDescriptorBody, ChannelsBlockSampleType, DataChannelPayloadDescriptor,
};

/// A message one of an island's inputs read off a channel, as it arrived, which
/// the island hands its reader on unread.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageAFunctionNodeIslandHandsOnUnread {
    /// The payload's bytes; an Image frame's payload is empty, its slot named
    /// by its descriptor.
    pub payload_bytes: Vec<u8>,
    /// What the payload is, exactly as its producer described it.
    pub payload_descriptor: DataChannelPayloadDescriptor,
    /// Its producer's publish stamp.
    pub timestamp_ns: i64,
    /// Whether it opens a group.
    pub group_boundary: MessageGroupBoundary,
    /// What its link lost between the message read before it and this one.
    pub lost_since_last_read: u64,
}

impl MessageAFunctionNodeIslandHandsOnUnread {
    /// `message` as an island holds it, its payload copied out of the sample
    /// it arrived in.
    pub fn read_off_its_channel(message: &MessageReadFromInputPort) -> Self {
        Self {
            payload_bytes: message.payload.to_vec(),
            payload_descriptor: message.payload_descriptor,
            timestamp_ns: message.timestamp_ns,
            group_boundary: message.group_boundary,
            lost_since_last_read: message.lost_since_last_read,
        }
    }
}

/// The value an island's output took for one read, by the family its port
/// carries.
#[derive(Debug, Clone, PartialEq)]
pub enum FunctionNodeIslandOutputValue {
    /// A number or a bool, carried as a control value.
    ControlValue(f64),
    /// A `dict`, as a Records message's msgpack payload.
    RecordsMessage(Vec<u8>),
    /// A message the island read off a channel and hands on unread.
    MessageHandedOnUnread(MessageAFunctionNodeIslandHandsOnUnread),
    /// Nothing at this read: the reader reads no message from the port.
    Nothing,
}

/// The value an island's output took for one read, and the reader's time it
/// was evaluated at.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionNodeIslandOutputForOneRead {
    /// The island output's value.
    pub value: FunctionNodeIslandOutputValue,
    /// The reader's stream-clock time the island was evaluated at.
    pub evaluated_at_ns: i64,
}

/// The island upstream of one input port, evaluated by the process that reads
/// the port each time it reads it.
pub trait FunctionNodeIslandEvaluatedAtEachRead: Send + Sync {
    /// Evaluate the island at the reader's current time, or hand back the
    /// value already evaluated at that time.
    fn evaluate_the_island_output_for_this_read(
        &self,
    ) -> Result<FunctionNodeIslandOutputForOneRead>;

    /// The first function node of the island that runs as Python, which only a
    /// read holding the Python interpreter's lock can call; `None` when every
    /// node runs as native code.
    fn the_first_function_node_it_runs_as_python(&self) -> Option<&str>;

    /// The state each stateful node of the island last returned, by its
    /// breadcrumb number, which an island replacing this one resumes from.
    fn the_state_each_stateful_function_node_last_returned(&self) -> Vec<(u32, Vec<f64>)>;
}

/// The pin value an input port reads while nothing is linked into it: the
/// island of no function nodes, stamped with the reader's time at each read.
pub(crate) struct PinValueReadByAnUnlinkedInputPort {
    pin_value: f64,
    stream_clock_now_ns: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl PinValueReadByAnUnlinkedInputPort {
    /// `pin_value`, read at the time `stream_clock_now_ns` gives.
    pub(crate) fn read_at_the_time_of(
        pin_value: f64,
        stream_clock_now_ns: Arc<dyn Fn() -> i64 + Send + Sync>,
    ) -> Self {
        Self {
            pin_value,
            stream_clock_now_ns,
        }
    }
}

impl FunctionNodeIslandEvaluatedAtEachRead for PinValueReadByAnUnlinkedInputPort {
    fn evaluate_the_island_output_for_this_read(
        &self,
    ) -> Result<FunctionNodeIslandOutputForOneRead> {
        Ok(FunctionNodeIslandOutputForOneRead {
            value: FunctionNodeIslandOutputValue::ControlValue(self.pin_value),
            evaluated_at_ns: (self.stream_clock_now_ns)(),
        })
    }

    fn the_first_function_node_it_runs_as_python(&self) -> Option<&str> {
        None
    }

    fn the_state_each_stateful_function_node_last_returned(&self) -> Vec<(u32, Vec<f64>)> {
        Vec::new()
    }
}

/// One input port an island feeds, and how many reads it has answered.
pub(crate) struct InputPortFedByAFunctionNodeIsland {
    island: Arc<dyn FunctionNodeIslandEvaluatedAtEachRead>,
    reads_answered: AtomicU64,
}

impl InputPortFedByAFunctionNodeIsland {
    pub(crate) fn evaluated_by(island: Arc<dyn FunctionNodeIslandEvaluatedAtEachRead>) -> Self {
        Self {
            island,
            reads_answered: AtomicU64::new(0),
        }
    }

    /// The port fed by `island` in place of the island that fed it before,
    /// counting reads on from where that one stopped, so the reader's sample
    /// positions never step back.
    pub(crate) fn evaluated_by_in_place_of(
        island: Arc<dyn FunctionNodeIslandEvaluatedAtEachRead>,
        island_port_it_replaces: &Self,
    ) -> Self {
        Self {
            island,
            reads_answered: AtomicU64::new(
                island_port_it_replaces
                    .reads_answered
                    .load(Ordering::Relaxed),
            ),
        }
    }

    /// The first function node the island runs as Python, if any.
    pub(crate) fn the_first_function_node_it_runs_as_python(&self) -> Option<&str> {
        self.island.the_first_function_node_it_runs_as_python()
    }

    /// The state each stateful node of the island last returned, by its
    /// breadcrumb number.
    pub(crate) fn the_state_each_stateful_function_node_last_returned(
        &self,
    ) -> Vec<(u32, Vec<f64>)> {
        self.island
            .the_state_each_stateful_function_node_last_returned()
    }

    /// The island's output for this read, or `None` when it hands out nothing.
    /// A control value is carried as a Channels block of one f64 sample on one
    /// channel, at rate 0, which states no regular rate; a `dict` as a Records
    /// message; a message handed on unread as it arrived.
    pub(crate) fn read_the_island_output(&self) -> Result<Option<MessageReadFromInputPort>> {
        let output = self.island.evaluate_the_island_output_for_this_read()?;
        let (payload_bytes, payload_descriptor) = match output.value {
            FunctionNodeIslandOutputValue::Nothing => return Ok(None),
            FunctionNodeIslandOutputValue::MessageHandedOnUnread(message) => {
                return Ok(Some(MessageReadFromInputPort {
                    payload: InputPortMessagePayload::OwnedBytes(message.payload_bytes),
                    timestamp_ns: message.timestamp_ns,
                    group_boundary: message.group_boundary,
                    lost_since_last_read: message.lost_since_last_read,
                    payload_descriptor: message.payload_descriptor,
                }));
            }
            FunctionNodeIslandOutputValue::ControlValue(control_value) => (
                control_value.to_le_bytes().to_vec(),
                DataChannelPayloadDescriptor::channels_block(ChannelsBlockDescriptorBody {
                    start_sample: self.reads_answered.fetch_add(1, Ordering::Relaxed),
                    sample_rate: 0,
                    channel_count: 1,
                    samples_per_channel: 1,
                    sample_type: ChannelsBlockSampleType::F64 as u32,
                }),
            ),
            FunctionNodeIslandOutputValue::RecordsMessage(records_message_payload) => {
                self.reads_answered.fetch_add(1, Ordering::Relaxed);
                (
                    records_message_payload,
                    DataChannelPayloadDescriptor::records_message(),
                )
            }
        };
        Ok(Some(MessageReadFromInputPort {
            payload: InputPortMessagePayload::OwnedBytes(payload_bytes),
            timestamp_ns: output.evaluated_at_ns,
            group_boundary: MessageGroupBoundary::StartsGroup,
            lost_since_last_read: 0,
            payload_descriptor,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct IslandAnsweringOneValue(FunctionNodeIslandOutputValue);

    impl FunctionNodeIslandEvaluatedAtEachRead for IslandAnsweringOneValue {
        fn evaluate_the_island_output_for_this_read(
            &self,
        ) -> Result<FunctionNodeIslandOutputForOneRead> {
            Ok(FunctionNodeIslandOutputForOneRead {
                value: self.0.clone(),
                evaluated_at_ns: 42,
            })
        }

        fn the_first_function_node_it_runs_as_python(&self) -> Option<&str> {
            None
        }

        fn the_state_each_stateful_function_node_last_returned(&self) -> Vec<(u32, Vec<f64>)> {
            Vec::new()
        }
    }

    #[test]
    fn a_control_value_is_one_f64_sample_on_one_channel_at_no_regular_rate() {
        let port = InputPortFedByAFunctionNodeIsland::evaluated_by(Arc::new(
            IslandAnsweringOneValue(FunctionNodeIslandOutputValue::ControlValue(0.25)),
        ));
        let message = port.read_the_island_output().unwrap().unwrap();
        assert_eq!(message.payload.bytes(), 0.25f64.to_le_bytes());
        assert_eq!(message.timestamp_ns, 42);
        let block =
            ChannelsBlockDescriptorBody::from_descriptor_body(&message.payload_descriptor.body);
        assert_eq!(block.sample_rate, 0);
        assert_eq!(block.channel_count, 1);
        assert_eq!(block.samples_per_channel, 1);
        assert_eq!(block.sample_type, ChannelsBlockSampleType::F64 as u32);
        assert_eq!(block.start_sample, 0);
        assert_eq!(
            ChannelsBlockDescriptorBody::from_descriptor_body(
                &port
                    .read_the_island_output()
                    .unwrap()
                    .unwrap()
                    .payload_descriptor
                    .body
            )
            .start_sample,
            1
        );
    }

    #[test]
    fn a_dict_is_one_records_message_carrying_its_msgpack_payload() {
        let records_message_payload = vec![0x81, 0xa1, b'k', 0x01];
        let port =
            InputPortFedByAFunctionNodeIsland::evaluated_by(Arc::new(IslandAnsweringOneValue(
                FunctionNodeIslandOutputValue::RecordsMessage(records_message_payload.clone()),
            )));
        let message = port.read_the_island_output().unwrap().unwrap();
        assert_eq!(message.payload.bytes(), records_message_payload);
        assert_eq!(message.timestamp_ns, 42);
        assert_eq!(
            message.payload_descriptor,
            DataChannelPayloadDescriptor::records_message()
        );
    }
}
