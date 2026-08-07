// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! ProcessorState enum and ECS component.

use serde::{Deserialize, Serialize};

/// State of a processor instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ProcessorState {
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
}

impl ProcessorState {
    /// Whether the processor has yet to finish `setup`.
    ///
    /// The two states a processor passes through before its thread runs
    /// `setup`: `Pending` from the moment it is added to the graph, `Idle` once
    /// the compiler has prepared it. Every later state means setup resolved —
    /// `Running` if it returned, `Error` if it raised — which is what makes
    /// this the readiness predicate: for a processor in a helper process,
    /// `setup` is the call that waits for the child to register and wire its
    /// ports.
    pub fn is_before_setup_completed(self) -> bool {
        matches!(self, Self::Pending | Self::Idle)
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

/// ECS component for processor state (attached to processor entities).
pub struct ProcessorStateComponent(pub ProcessorState);
