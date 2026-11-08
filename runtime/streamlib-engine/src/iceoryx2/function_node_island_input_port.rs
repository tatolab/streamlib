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

/// The value an island's output took for one read, and the reader's time it
/// was evaluated at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FunctionNodeIslandOutputForOneRead {
    /// The island output's value.
    pub value: f64,
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

    /// The island's output for this read, as a control value on a wire is
    /// carried: a Channels block of one f64 sample on one channel, at rate 0,
    /// which states no regular rate.
    pub(crate) fn read_the_island_output(&self) -> Result<MessageReadFromInputPort> {
        let output = self.island.evaluate_the_island_output_for_this_read()?;
        let block = ChannelsBlockDescriptorBody {
            start_sample: self.reads_answered.fetch_add(1, Ordering::Relaxed),
            sample_rate: 0,
            channel_count: 1,
            samples_per_channel: 1,
            sample_type: ChannelsBlockSampleType::F64 as u32,
        };
        Ok(MessageReadFromInputPort {
            payload: InputPortMessagePayload::OwnedBytes(output.value.to_le_bytes().to_vec()),
            timestamp_ns: output.evaluated_at_ns,
            group_boundary: MessageGroupBoundary::StartsGroup,
            lost_since_last_read: 0,
            payload_descriptor: DataChannelPayloadDescriptor::channels_block(block),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct IslandAnsweringOneValue(f64);

    impl FunctionNodeIslandEvaluatedAtEachRead for IslandAnsweringOneValue {
        fn evaluate_the_island_output_for_this_read(
            &self,
        ) -> Result<FunctionNodeIslandOutputForOneRead> {
            Ok(FunctionNodeIslandOutputForOneRead {
                value: self.0,
                evaluated_at_ns: 42,
            })
        }
    }

    #[test]
    fn a_read_is_one_f64_sample_on_one_channel_at_no_regular_rate() {
        let port = InputPortFedByAFunctionNodeIsland::evaluated_by(Arc::new(
            IslandAnsweringOneValue(0.25),
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
}
