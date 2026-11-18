// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The config defaults the conversion nodes share, so a node placed with no
//! config takes its config and runs.

/// The one channel a conversion node names when its config names none.
pub(crate) fn one_channel_named_value() -> Vec<String> {
    vec!["value".to_string()]
}

/// The sample rate a conversion node's Channels output states when its config states none.
pub(crate) fn forty_eight_thousand_samples_per_second() -> u32 {
    48_000
}
