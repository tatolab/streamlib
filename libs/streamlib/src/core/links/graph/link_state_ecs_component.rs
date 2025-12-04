//! LinkState and LinkStateComponent - ECS component for link state.

use serde::{Deserialize, Serialize};

/// State of a link in the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LinkState {
    /// Link exists in graph but not yet wired (pending commit).
    #[default]
    Pending,
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
            Self::Wired => write!(f, "Wired"),
            Self::Disconnecting => write!(f, "Disconnecting"),
            Self::Disconnected => write!(f, "Disconnected"),
            Self::Error => write!(f, "Error"),
        }
    }
}

/// ECS component for link state (attached to link entities).
pub struct LinkStateComponent(pub LinkState);

impl Default for LinkStateComponent {
    fn default() -> Self {
        Self(LinkState::Pending)
    }
}
