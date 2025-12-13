// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};

use super::PortInfo;

/// Container for a node's input and output ports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProcessorNodePorts {
    pub inputs: Vec<PortInfo>,
    pub outputs: Vec<PortInfo>,
}
