// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use serde_json::Value as JsonValue;

use super::{Iceoryx2ServicesHeldOpenForLinkComponent, JsonSerializableComponent};
use crate::core::graph::{InputLinkPortRef, LinkUniqueId};
use crate::core::processors::OutOfProcessLinkWireReply;

/// Marks a link from a node outside any island into a function node whose
/// channel is read by the readers of the islands calling that node, once for
/// each island link, at an input port of that link's reader opened for it;
/// `graph` renders those ports as
/// `read_through_the_reader_of_its_function_node_island`.
pub struct LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent {
    /// Each read of the link's channel, one per island link evaluating the
    /// function node it feeds.
    pub reads_by_island_link: Vec<ReadOfALinkByTheReaderOfAFunctionNodeIsland>,
}

/// One subscription to a link's channel, held by the reader an island link
/// carries its island to.
pub struct ReadOfALinkByTheReaderOfAFunctionNodeIsland {
    /// The link from the island into its reader that evaluates the function
    /// node the channel feeds.
    pub island_link_id: LinkUniqueId,
    /// The reader's input port the channel is wired into.
    pub reader_input_port: InputLinkPortRef,
    /// The identity the read is wired under on both ends of the channel, so
    /// the source holds one notifier per read.
    pub wiring_link_id: LinkUniqueId,
    /// The answers the out-of-process ends this read was handed to owe.
    pub wire_replies_awaited: Vec<Arc<OutOfProcessLinkWireReply>>,
    /// The services this read holds open for as long as it is wired.
    pub services_held_open: Iceoryx2ServicesHeldOpenForLinkComponent,
}

impl LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent {
    /// The read the reader of `island_link_id` holds, if it holds one.
    pub fn the_read_for_the_island_link(
        &self,
        island_link_id: &LinkUniqueId,
    ) -> Option<&ReadOfALinkByTheReaderOfAFunctionNodeIsland> {
        self.reads_by_island_link
            .iter()
            .find(|read| &read.island_link_id == island_link_id)
    }
}

impl JsonSerializableComponent for LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent {
    fn json_key(&self) -> &'static str {
        "read_through_the_reader_of_its_function_node_island"
    }

    fn to_json(&self) -> JsonValue {
        let mut reader_input_ports: Vec<String> = self
            .reads_by_island_link
            .iter()
            .map(|read| read.reader_input_port.to_string())
            .collect();
        reader_input_ports.sort();
        JsonValue::from(reader_input_ports)
    }
}
