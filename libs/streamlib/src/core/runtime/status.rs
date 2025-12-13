// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/// Runtime lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RuntimeStatus {
    #[default]
    Initial,
    Starting,
    Started,
    Stopping,
    Stopped,
    Pausing,
    Paused,
}
