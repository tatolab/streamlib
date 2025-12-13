// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::fmt;

/// Compilation phase in the 4-phase pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompilePhase {
    /// Phase 1: Creating processor instances via factory.
    Create,
    /// Phase 2: Wiring links between processors (ring buffers).
    Wire,
    /// Phase 3: Setting up processors (GPU, devices).
    Setup,
    /// Phase 4: Starting processor threads.
    Start,
}

impl CompilePhase {
    /// All phases in execution order.
    pub const ALL: [CompilePhase; 4] = [
        CompilePhase::Create,
        CompilePhase::Wire,
        CompilePhase::Setup,
        CompilePhase::Start,
    ];

    /// Get the next phase, if any.
    pub fn next(self) -> Option<Self> {
        match self {
            Self::Create => Some(Self::Wire),
            Self::Wire => Some(Self::Setup),
            Self::Setup => Some(Self::Start),
            Self::Start => None,
        }
    }

    /// Get the phase number (1-4).
    pub fn number(self) -> u8 {
        match self {
            Self::Create => 1,
            Self::Wire => 2,
            Self::Setup => 3,
            Self::Start => 4,
        }
    }

    /// Get a human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Create => "CREATE",
            Self::Wire => "WIRE",
            Self::Setup => "SETUP",
            Self::Start => "START",
        }
    }
}

impl fmt::Display for CompilePhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Phase {}: {}", self.number(), self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_phase_ordering() {
        assert_eq!(CompilePhase::Create.next(), Some(CompilePhase::Wire));
        assert_eq!(CompilePhase::Wire.next(), Some(CompilePhase::Setup));
        assert_eq!(CompilePhase::Setup.next(), Some(CompilePhase::Start));
        assert_eq!(CompilePhase::Start.next(), None);
    }

    #[test]
    fn test_phase_numbers() {
        assert_eq!(CompilePhase::Create.number(), 1);
        assert_eq!(CompilePhase::Wire.number(), 2);
        assert_eq!(CompilePhase::Setup.number(), 3);
        assert_eq!(CompilePhase::Start.number(), 4);
    }

    #[test]
    fn test_phase_display() {
        assert_eq!(CompilePhase::Create.to_string(), "Phase 1: CREATE");
        assert_eq!(CompilePhase::Start.to_string(), "Phase 4: START");
    }
}
