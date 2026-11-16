// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::fmt;

/// Compilation phase in the 3-phase pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum CompilePhase {
    Prepare,
    Spawn,
    Wire,
}

impl CompilePhase {
    /// Get the phase number (1-3).
    pub(super) fn number(self) -> u8 {
        match self {
            Self::Prepare => 1,
            Self::Spawn => 2,
            Self::Wire => 3,
        }
    }

    /// Get a human-readable name.
    pub(super) fn name(self) -> &'static str {
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
