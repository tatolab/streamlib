// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Read mode for frame consumption from input ports.

use serde::{Deserialize, Serialize};

/// How frames should be read from an input port's buffer.
///
/// No longer an authoring knob — it is the consumer-side drain order a
/// [`DeliveryProfile`](crate::iceoryx2::DeliveryProfile) resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadMode {
    /// Drain buffer and return only the newest frame (optimal for video).
    #[default]
    SkipToLatest,
    /// Read next frame in FIFO order (required for audio).
    ReadNextInOrder,
}

impl ReadMode {
    /// Whether a bag a port read this way never hands its reader is a loss.
    ///
    /// A `newest` port passing over bags to reach the most recent is the
    /// profile working; an `ordered` port promised every bag in turn.
    pub fn a_bag_passed_over_is_lost(self) -> bool {
        match self {
            ReadMode::SkipToLatest => false,
            ReadMode::ReadNextInOrder => true,
        }
    }

    /// The canonical manifest/envelope string — the wire form the subprocess
    /// SDKs map back to their `*_input_set_read_mode` integer.
    pub fn as_manifest_str(self) -> &'static str {
        match self {
            ReadMode::SkipToLatest => "skip_to_latest",
            ReadMode::ReadNextInOrder => "read_next_in_order",
        }
    }
}
