// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A node that writes one-message groups two at a time, for the tests of a
//! relay reader: the `n`th message is the bytes `message <n>`, stamped `10 n`
//! milliseconds, each opening a group, and every 20 ms it writes the next two
//! back to back, so two groups are ready on the wire together.
//! Compiled into a test build alone.

use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::error::Result;
use streamlib::sdk::iceoryx2::MessageGroupBoundary;
use streamlib::sdk::processors::{ContinuousProcessor, PROCESSOR_REGISTRY};

/// The stamp of the message after one.
const NANOSECONDS_BETWEEN_MESSAGE_STAMPS: i64 = 10_000_000;

/// How many one-message groups each write puts on the wire together.
const GROUPS_WRITTEN_TOGETHER: u64 = 2;

#[streamlib::sdk::processor(
    execution = continuous(interval_ms = 20),
    output("paired_groups"),
)]
pub struct PairedGroupWriterTestNode {
    next_message_number: u64,
}

impl ContinuousProcessor for PairedGroupWriterTestNode::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        for _ in 0..GROUPS_WRITTEN_TOGETHER {
            let message_number = self.next_message_number;
            self.next_message_number += 1;
            self.outputs.write_bytes_with_timestamp(
                "paired_groups",
                format!("message {message_number}").as_bytes(),
                message_number as i64 * NANOSECONDS_BETWEEN_MESSAGE_STAMPS,
                MessageGroupBoundary::StartsGroup,
            )?;
        }
        Ok(())
    }
}

/// Register the node on the process-wide registry.
pub(crate) fn register_the_paired_group_writer_test_node() {
    PROCESSOR_REGISTRY.register::<PairedGroupWriterTestNode::Processor>();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_graph_names_the_node_by_its_class_import_path() {
        assert_eq!(
            PairedGroupWriterTestNode::processor_class_import_path().as_str(),
            "tatolabd::paired_group_writer_test_node::PairedGroupWriterTestNode"
        );
    }
}
