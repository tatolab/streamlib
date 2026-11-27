// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node that writes numbered messages, for the tests of the publisher: the
//! `n`th message is the bytes `message <n>`, stamped `10 n` milliseconds — the
//! interval it writes at — and opens a group when `n` is a multiple of three.
//! It writes each one on both of its ports, `numbers` and `more_numbers`.
//! Compiled into a test build alone.

use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::error::Result;
use streamlib::sdk::iceoryx2::{MessageGroupBoundary, OutputWriter};
use streamlib::sdk::nodes::NODE_REGISTRY;

/// Every how many messages one opens a group.
pub(crate) const MESSAGES_PER_GROUP: u64 = 3;

/// The stamp of the message after one, the interval the node writes at.
pub(crate) const NANOSECONDS_BETWEEN_MESSAGE_STAMPS: i64 = 10_000_000;

#[streamlib::sdk::node(
    execution = continuous(interval_ms = 10),
    output("numbers", pin_type = String, flow = Stream),
    output("more_numbers", pin_type = String, flow = Stream),
)]
pub(crate) fn numbered_message_writer_test_node(
    outputs: OutputWriter,
) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
    let mut next_message_number = 0u64;
    move |_ctx| {
        let message_number = next_message_number;
        next_message_number += 1;
        let group_boundary = if message_number.is_multiple_of(MESSAGES_PER_GROUP) {
            MessageGroupBoundary::StartsGroup
        } else {
            MessageGroupBoundary::ContinuesGroup
        };
        for port in ["numbers", "more_numbers"] {
            outputs.write_bytes_with_timestamp(
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
    NODE_REGISTRY.register::<NumberedMessageWriterTestNode::Node>();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_graph_names_the_node_by_its_class_import_path() {
        assert_eq!(
            NumberedMessageWriterTestNode::node_class_import_path().as_str(),
            "tatolabd::numbered_message_writer_test_node::NumberedMessageWriterTestNode"
        );
    }
}
