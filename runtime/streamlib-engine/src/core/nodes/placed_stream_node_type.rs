// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a placed stream declares: its one setting, `stream`, the project's stream it places, as
//! an input. Its other inputs and its outputs are the Inputs and Outputs that stream declares.

use std::sync::LazyLock;

use streamlib_project_repository::STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY;

use crate::core::descriptors::{PinType, PortDescriptor, ProjectAssetMedia};

use super::graph_interface_node_types::setting;

/// The settings of a placed stream, built once.
static THE_SETTINGS_OF_A_PLACED_STREAM: LazyLock<Vec<PortDescriptor>> = LazyLock::new(|| {
    vec![setting(
        STREAM_FRAGMENT_HELD_STREAM_CONFIG_KEY,
        "The stream it places: one of the project's streams, or a stream of a project it \
         imports.",
        PinType::Asset {
            media: vec![ProjectAssetMedia::Stream],
        },
        false,
        None,
    )]
});

/// The settings a placed stream takes, as its type declares them: `stream`, an Asset that takes
/// only a stream asset, read at setup.
pub(crate) fn the_settings_of_a_placed_stream() -> &'static [PortDescriptor] {
    &THE_SETTINGS_OF_A_PLACED_STREAM
}
