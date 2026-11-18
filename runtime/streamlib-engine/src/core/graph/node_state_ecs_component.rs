// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};
use streamlib_runtime_client_contract::local_api_wire_contract::NodeStateOnTheWire;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum NodeState {
    /// Waiting to be started (registered but not yet running).
    #[default]
    Pending,
    /// Prepared by the compiler, but its thread has not run `setup` yet.
    Idle,
    /// Actively processing frames.
    Running,
    /// Temporarily paused (resources still allocated).
    Paused,
    /// In the process of shutting down.
    Stopping,
    /// Fully stopped and cleaned up.
    Stopped,
    /// Error state (processing failed).
    Error,
    /// Its helper process died on its own and a new one starts after the
    /// restart backoff.
    Restarting,
}

impl NodeState {
    pub fn is_before_setup_completed(self) -> bool {
        matches!(self, Self::Pending | Self::Idle)
    }

    pub fn holds_a_not_ready_report(self) -> bool {
        matches!(
            self,
            Self::Pending | Self::Idle | Self::Running | Self::Paused
        )
    }
}

impl std::fmt::Display for NodeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "Pending"),
            Self::Idle => write!(f, "Idle"),
            Self::Running => write!(f, "Running"),
            Self::Paused => write!(f, "Paused"),
            Self::Stopping => write!(f, "Stopping"),
            Self::Stopped => write!(f, "Stopped"),
            Self::Error => write!(f, "Error"),
            Self::Restarting => write!(f, "Restarting"),
        }
    }
}

impl From<NodeState> for NodeStateOnTheWire {
    fn from(node_state: NodeState) -> Self {
        match node_state {
            NodeState::Pending => Self::Pending,
            NodeState::Idle => Self::Idle,
            NodeState::Running => Self::Running,
            NodeState::Paused => Self::Paused,
            NodeState::Stopping => Self::Stopping,
            NodeState::Stopped => Self::Stopped,
            NodeState::Error => Self::Error,
            NodeState::Restarting => Self::Restarting,
        }
    }
}

pub struct NodeStateComponent(pub NodeState);
