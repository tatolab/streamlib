// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Link buffer read mode for reading from link ports.

use serde::{Deserialize, Serialize};

/// How a frame type should be read from the link buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkBufferReadMode {
    /// Drain buffer and return only the newest frame (optimal for video).
    SkipToLatest,
    /// Read next frame in FIFO order (required for audio).
    ReadNextInOrder,
}
