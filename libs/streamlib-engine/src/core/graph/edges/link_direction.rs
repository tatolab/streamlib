// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use petgraph::Direction;
use serde::{Deserialize, Serialize};

/// Direction of a port in a link endpoint
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkDirection {
    /// Input port (receives data)
    Input,
    /// Output port (sends data)
    Output,
}

impl From<LinkDirection> for Direction {
    fn from(dir: LinkDirection) -> Self {
        match dir {
            LinkDirection::Input => Direction::Incoming,
            LinkDirection::Output => Direction::Outgoing,
        }
    }
}

impl From<Direction> for LinkDirection {
    fn from(dir: Direction) -> Self {
        match dir {
            Direction::Incoming => LinkDirection::Input,
            Direction::Outgoing => LinkDirection::Output,
        }
    }
}
