// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::BTreeSet;

/// The output ports of this node its stream wired into an input on another
/// runtime, each with that runtime's name.
///
/// A stream's own wiring runs whatever the port's exposure: the runtime named
/// here may read the port, and no other runtime gains anything from it. Held on
/// the node so a removed node takes its wiring with it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputPortsWiredToAnotherRuntimeComponent(pub BTreeSet<OutputPortWiredToAnotherRuntime>);

/// One output port wired into an input on the runtime `input_runtime_name`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OutputPortWiredToAnotherRuntime {
    /// The output port's name on this node.
    pub port_name: String,
    /// The name of the runtime holding the input it is wired into.
    pub input_runtime_name: String,
}

impl OutputPortsWiredToAnotherRuntimeComponent {
    /// Whether the stream wired `port_name` into an input on `input_runtime_name`.
    pub fn wires_into(&self, port_name: &str, input_runtime_name: &str) -> bool {
        self.0.iter().any(|wired| {
            wired.port_name == port_name && wired.input_runtime_name == input_runtime_name
        })
    }
}
