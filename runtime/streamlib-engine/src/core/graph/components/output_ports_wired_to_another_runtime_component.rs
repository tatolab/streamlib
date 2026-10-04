// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::BTreeSet;

use crate::core::graph::LinkRequestUniqueId;

/// The output ports of this node its stream asked another runtime to wire into
/// one of its inputs — each of those runtimes may read the port whatever its
/// exposure.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputPortsWiredToAnotherRuntimeComponent(pub BTreeSet<OutputPortWiredToAnotherRuntime>);

/// One output port the request `link_request_id` asked the runtime
/// `input_runtime_name` to wire into one of its inputs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OutputPortWiredToAnotherRuntime {
    /// The output port's name on this node.
    pub port_name: String,
    /// The name of the runtime holding the input it is wired into.
    pub input_runtime_name: String,
    /// The request that asked for the link.
    pub link_request_id: LinkRequestUniqueId,
}

impl OutputPortsWiredToAnotherRuntimeComponent {
    /// Whether the stream wired `port_name` into an input on `input_runtime_name`.
    pub fn wires_into(&self, port_name: &str, input_runtime_name: &str) -> bool {
        self.0.iter().any(|wired| {
            wired.port_name == port_name && wired.input_runtime_name == input_runtime_name
        })
    }

    /// Forget the wiring the request `link_request_id` asked for.
    pub fn forget_the_wiring_requested_by(&mut self, link_request_id: &LinkRequestUniqueId) {
        self.0
            .retain(|wired| &wired.link_request_id != link_request_id);
    }
}
