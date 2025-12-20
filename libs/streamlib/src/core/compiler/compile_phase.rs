// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::fmt;

/// Compilation phase in the 3-phase pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompilePhase {
    /// Phase 1: Attaching infrastructure components to processor nodes.
    Prepare,
    /// Phase 2: Spawning processor threads (threads create instances).
    Spawn,
    /// Phase 3: Wiring links between processors (ring buffers).
    Wire,
}

impl CompilePhase {
    /// All phases in execution order.
    pub const ALL: [CompilePhase; 3] = [
        CompilePhase::Prepare,
        CompilePhase::Spawn,
        CompilePhase::Wire,
    ];

    /// Get the next phase, if any.
    pub fn next(self) -> Option<Self> {
        match self {
            Self::Prepare => Some(Self::Spawn),
            Self::Spawn => Some(Self::Wire),
            Self::Wire => None,
        }
    }

    /// Get the phase number (1-3).
    pub fn number(self) -> u8 {
        match self {
            Self::Prepare => 1,
            Self::Spawn => 2,
            Self::Wire => 3,
        }
    }

    /// Get a human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Prepare => "PREPARE",
            Self::Spawn => "SPAWN",
            Self::Wire => "WIRE",
        }
    }
}

impl fmt::Display for CompilePhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Phase {}: {}", self.number(), self.name())
    }
}
