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

/// The value an island's output took for one read, by the family its port
/// carries.
#[derive(Debug, Clone, PartialEq)]
pub enum FunctionNodeIslandOutputValue {
    /// A number or a bool, carried as a control value.
    ControlValue(f64),
    /// A `dict`, as a Records message's msgpack payload.
    RecordsMessage(Vec<u8>),
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

    /// The island's output for this read. A control value is carried as a
    /// Channels block of one f64 sample on one channel, at rate 0, which states
    /// no regular rate; a `dict` as a Records message.
    pub(crate) fn read_the_island_output(&self) -> Result<MessageReadFromInputPort> {
        let output = self.island.evaluate_the_island_output_for_this_read()?;
        let reads_answered_before = self.reads_answered.fetch_add(1, Ordering::Relaxed);
        let (payload_bytes, payload_descriptor) = match output.value {
            FunctionNodeIslandOutputValue::ControlValue(control_value) => (
                control_value.to_le_bytes().to_vec(),
                DataChannelPayloadDescriptor::channels_block(ChannelsBlockDescriptorBody {
                    start_sample: reads_answered_before,
                    sample_rate: 0,
                    channel_count: 1,
                    samples_per_channel: 1,
                    sample_type: ChannelsBlockSampleType::F64 as u32,
                }),
            ),
            FunctionNodeIslandOutputValue::RecordsMessage(records_message_payload) => (
                records_message_payload,
                DataChannelPayloadDescriptor::records_message(),
            ),
        };
        Ok(MessageReadFromInputPort {
            payload: InputPortMessagePayload::OwnedBytes(payload_bytes),
            timestamp_ns: output.evaluated_at_ns,
            group_boundary: MessageGroupBoundary::StartsGroup,
            lost_since_last_read: 0,
            payload_descriptor,
        })
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
        let message = port.read_the_island_output().unwrap();
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
        let message = port.read_the_island_output().unwrap();
        assert_eq!(message.payload.bytes(), records_message_payload);
        assert_eq!(message.timestamp_ns, 42);
        assert_eq!(
            message.payload_descriptor,
            DataChannelPayloadDescriptor::records_message()
        );
    }
}
