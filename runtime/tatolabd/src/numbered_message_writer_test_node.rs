// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A node that writes numbered messages, for the tests of the publisher: the
//! `n`th message is the bytes `message <n>`, stamped `10 n` milliseconds — the
//! interval it writes at — and opens a group when `n` is a multiple of three.
//! It writes each one on both of its ports, `numbers` declaring a catalog entry
//! and `more_numbers` none.
//! Compiled into a test build alone.

use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::error::Result;
use streamlib::sdk::iceoryx2::MessageGroupBoundary;
use streamlib::sdk::processors::{ContinuousProcessor, PROCESSOR_REGISTRY};

/// Every how many messages one opens a group.
pub const MESSAGES_PER_GROUP: u64 = 3;

/// The stamp of the message after one, the interval the node writes at.
pub const NANOSECONDS_BETWEEN_MESSAGE_STAMPS: i64 = 10_000_000;

#[streamlib::sdk::processor(
    execution = continuous(interval_ms = 10),
    output(
        "numbers",
        catalog = BinaryEntry { mode: Stream, mime: "text/plain" }
    ),
    output("more_numbers"),
)]
pub struct NumberedMessageWriterTestNode {
    next_message_number: u64,
}

impl ContinuousProcessor for NumberedMessageWriterTestNode::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        let message_number = self.next_message_number;
        self.next_message_number += 1;
        let group_boundary = if message_number.is_multiple_of(MESSAGES_PER_GROUP) {
            MessageGroupBoundary::StartsGroup
        } else {
            MessageGroupBoundary::ContinuesGroup
        };
        for port in ["numbers", "more_numbers"] {
            self.outputs.write_bytes_with_timestamp(
                port,
                format!("message {message_number}").as_bytes(),
                message_number as i64 * NANOSECONDS_BETWEEN_MESSAGE_STAMPS,
                group_boundary,
            )?;
        }
        Ok(())
    }
}

/// Register the node on the process-wide registry.
pub(crate) fn register_the_numbered_message_writer_test_node() {
    PROCESSOR_REGISTRY.register::<NumberedMessageWriterTestNode::Processor>();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_graph_names_the_node_by_its_class_import_path() {
        assert_eq!(
            NumberedMessageWriterTestNode::processor_class_import_path().as_str(),
            "tatolabd::numbered_message_writer_test_node::NumberedMessageWriterTestNode"
        );
    }
}
