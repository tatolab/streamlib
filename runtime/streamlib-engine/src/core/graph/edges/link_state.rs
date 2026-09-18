// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};

/// State of a link in the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LinkState {
    /// Link exists in graph but not yet wired (pending commit).
    #[default]
    Pending,
    /// The link's source is a port on another runtime, and nothing carries yet
    /// — the runtime is not on the mesh, or is and does not offer the port.
    /// The reason says which; `graph` renders it beside the state.
    AwaitingRemote,
    /// Link is actively wired with a ring buffer channel.
    Wired,
    /// Link is being disconnected.
    Disconnecting,
    /// Link was disconnected (will be removed from graph).
    Disconnected,
    /// Link is in error state (wiring failed).
    Error,
}

impl std::fmt::Display for LinkState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "Pending"),
            Self::AwaitingRemote => write!(f, "AwaitingRemote"),
            Self::Wired => write!(f, "Wired"),
            Self::Disconnecting => write!(f, "Disconnecting"),
            Self::Disconnected => write!(f, "Disconnected"),
            Self::Error => write!(f, "Error"),
        }
    }
}
