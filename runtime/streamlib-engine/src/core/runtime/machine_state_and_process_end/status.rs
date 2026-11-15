// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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
