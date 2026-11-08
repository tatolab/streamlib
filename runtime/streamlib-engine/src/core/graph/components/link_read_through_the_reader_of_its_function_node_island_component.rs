// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;
use crate::core::graph::InputLinkPortRef;

/// Marks a link from a node outside any island into a function node whose
/// channel is read by the island's reader, at the input port of that reader
/// it opened for the link; `graph` renders that port as
/// `read_through_the_reader_of_its_function_node_island`.
pub struct LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent {
    /// The reader's input port the link's channel is wired into.
    pub reader_input_port: InputLinkPortRef,
}

impl JsonSerializableComponent for LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent {
    fn json_key(&self) -> &'static str {
        "read_through_the_reader_of_its_function_node_island"
    }

    fn to_json(&self) -> JsonValue {
        JsonValue::from(self.reader_input_port.to_string())
    }
}
