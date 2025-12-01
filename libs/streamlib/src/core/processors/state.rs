use serde::{Deserialize, Serialize};

/// State of a processor instance
///
/// This enum represents the lifecycle states a processor can be in.
/// Used by the executor for internal tracking and published via the event bus
/// for external observability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProcessorState {
    /// Waiting to be started (registered but not yet running)
    Pending,
    /// Setup complete, ready to process but not yet active
    Idle,
    /// Actively processing frames
    Running,
    /// Temporarily paused (resources still allocated)
    Paused,
    /// In the process of shutting down
    Stopping,
    /// Fully stopped and cleaned up
    Stopped,
    /// Error state (processing failed)
    Error,
}

impl Default for ProcessorState {
    fn default() -> Self {
        Self::Pending
    }
}

impl std::fmt::Display for ProcessorState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "Pending"),
            Self::Idle => write!(f, "Idle"),
            Self::Running => write!(f, "Running"),
            Self::Paused => write!(f, "Paused"),
            Self::Stopping => write!(f, "Stopping"),
            Self::Stopped => write!(f, "Stopped"),
            Self::Error => write!(f, "Error"),
        }
    }
}
