// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Read mode for frame consumption from input ports.

use serde::{Deserialize, Serialize};

/// How frames should be read from an input port's buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadMode {
    /// Drain buffer and return only the newest frame (optimal for video).
    #[default]
    SkipToLatest,
    /// Read next frame in FIFO order (required for audio).
    ReadNextInOrder,
}
