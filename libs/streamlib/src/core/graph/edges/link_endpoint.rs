// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};

use super::super::ProcessorUniqueId;
use super::LinkDirection;

/// One endpoint of a link (source or target)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkEndpoint {
    /// Node (processor) ID
    pub node: ProcessorUniqueId,
    /// Port name on the node
    pub port: String,
    /// Direction of this port
    pub direction: LinkDirection,
}

impl LinkEndpoint {
    /// Create a new source endpoint (output port)
    pub fn source(node: impl Into<ProcessorUniqueId>, port: impl Into<String>) -> Self {
        Self {
            node: node.into(),
            port: port.into(),
            direction: LinkDirection::Output,
        }
    }

    /// Create a new target endpoint (input port)
    pub fn target(node: impl Into<ProcessorUniqueId>, port: impl Into<String>) -> Self {
        Self {
            node: node.into(),
            port: port.into(),
            direction: LinkDirection::Input,
        }
    }

    /// Convert to port address format "node.port"
    pub fn to_address(&self) -> String {
        format!("{}.{}", self.node, self.port)
    }
}
