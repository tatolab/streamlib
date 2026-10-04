// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/// The output ports of this node its stream exposes, by port name.
///
/// Held on the node so a removed node takes its exposures with it. Stored but
/// never rendered under the node's `components`: `graph` renders every
/// exposure once, in its top-level `exposed`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExposedOutputPortsComponent(pub Vec<String>);

impl ExposedOutputPortsComponent {
    /// Whether the stream exposes this node's output port `port_name`.
    pub fn exposes(&self, port_name: &str) -> bool {
        self.0
            .iter()
            .any(|exposed_port_name| exposed_port_name == port_name)
    }
}
