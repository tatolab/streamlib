// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The Core Nodes inputs that became one: each Color that was three numbers, and each Vector
//! that was two. Each part keeps the default its old input declared, and a Color's alpha, which
//! no old input fed, is opaque.

use super::{MergedInput, MergedInto, MergedPart};

/// Each input of a node type that became one input.
pub(in crate::stream_document_upgrade) const MERGED_INPUTS: &[MergedInput] = &[
    // Tint's three numbers, one Color with the alpha it never had.
    MergedInput {
        node_type: "core_nodes.color:Tint",
        input: "color",
        merged_into: MergedInto::Color,
        parts: &[
            MergedPart::FedBy {
                old_input: "red",
                default: 1.0,
            },
            MergedPart::FedBy {
                old_input: "green",
                default: 0.5,
            },
            MergedPart::FedBy {
                old_input: "blue",
                default: 0.2,
            },
            MergedPart::Fixed(1.0),
        ],
    },
    // Duotone's shadow and highlight colours, three numbers each.
    MergedInput {
        node_type: "core_nodes.color:Duotone",
        input: "dark",
        merged_into: MergedInto::Color,
        parts: &[
            MergedPart::FedBy {
                old_input: "dark_red",
                default: 0.05,
            },
            MergedPart::FedBy {
                old_input: "dark_green",
                default: 0.0,
            },
            MergedPart::FedBy {
                old_input: "dark_blue",
                default: 0.3,
            },
            MergedPart::Fixed(1.0),
        ],
    },
    MergedInput {
        node_type: "core_nodes.color:Duotone",
        input: "light",
        merged_into: MergedInto::Color,
        parts: &[
            MergedPart::FedBy {
                old_input: "light_red",
                default: 1.0,
            },
            MergedPart::FedBy {
                old_input: "light_green",
                default: 0.8,
            },
            MergedPart::FedBy {
                old_input: "light_blue",
                default: 0.4,
            },
            MergedPart::Fixed(1.0),
        ],
    },
    // Chroma Key's key colour.
    MergedInput {
        node_type: "core_nodes.filters:ChromaKey",
        input: "key",
        merged_into: MergedInto::Color,
        parts: &[
            MergedPart::FedBy {
                old_input: "key_red",
                default: 0.0,
            },
            MergedPart::FedBy {
                old_input: "key_green",
                default: 1.0,
            },
            MergedPart::FedBy {
                old_input: "key_blue",
                default: 0.0,
            },
            MergedPart::Fixed(1.0),
        ],
    },
    // Gradient's two colours.
    MergedInput {
        node_type: "core_nodes.generators:Gradient",
        input: "start",
        merged_into: MergedInto::Color,
        parts: &[
            MergedPart::FedBy {
                old_input: "start_red",
                default: 0.0,
            },
            MergedPart::FedBy {
                old_input: "start_green",
                default: 0.0,
            },
            MergedPart::FedBy {
                old_input: "start_blue",
                default: 0.0,
            },
            MergedPart::Fixed(1.0),
        ],
    },
    MergedInput {
        node_type: "core_nodes.generators:Gradient",
        input: "end",
        merged_into: MergedInto::Color,
        parts: &[
            MergedPart::FedBy {
                old_input: "end_red",
                default: 1.0,
            },
            MergedPart::FedBy {
                old_input: "end_green",
                default: 1.0,
            },
            MergedPart::FedBy {
                old_input: "end_blue",
                default: 1.0,
            },
            MergedPart::Fixed(1.0),
        ],
    },
    // Radial Gradient's centre and two colours.
    MergedInput {
        node_type: "core_nodes.generators:RadialGradient",
        input: "center",
        merged_into: MergedInto::Vector,
        parts: &[
            MergedPart::FedBy {
                old_input: "center_x",
                default: 0.5,
            },
            MergedPart::FedBy {
                old_input: "center_y",
                default: 0.5,
            },
        ],
    },
    MergedInput {
        node_type: "core_nodes.generators:RadialGradient",
        input: "inner",
        merged_into: MergedInto::Color,
        parts: &[
            MergedPart::FedBy {
                old_input: "inner_red",
                default: 1.0,
            },
            MergedPart::FedBy {
                old_input: "inner_green",
                default: 1.0,
            },
            MergedPart::FedBy {
                old_input: "inner_blue",
                default: 1.0,
            },
            MergedPart::Fixed(1.0),
        ],
    },
    MergedInput {
        node_type: "core_nodes.generators:RadialGradient",
        input: "outer",
        merged_into: MergedInto::Color,
        parts: &[
            MergedPart::FedBy {
                old_input: "outer_red",
                default: 0.0,
            },
            MergedPart::FedBy {
                old_input: "outer_green",
                default: 0.0,
            },
            MergedPart::FedBy {
                old_input: "outer_blue",
                default: 0.0,
            },
            MergedPart::Fixed(1.0),
        ],
    },
    // Checkerboard's scroll.
    MergedInput {
        node_type: "core_nodes.generators:Checkerboard",
        input: "scroll",
        merged_into: MergedInto::Vector,
        parts: &[
            MergedPart::FedBy {
                old_input: "scroll_x",
                default: 0.0,
            },
            MergedPart::FedBy {
                old_input: "scroll_y",
                default: 0.0,
            },
        ],
    },
    // Circle's centre and colour.
    MergedInput {
        node_type: "core_nodes.generators:Circle",
        input: "center",
        merged_into: MergedInto::Vector,
        parts: &[
            MergedPart::FedBy {
                old_input: "center_x",
                default: 0.5,
            },
            MergedPart::FedBy {
                old_input: "center_y",
                default: 0.5,
            },
        ],
    },
    MergedInput {
        node_type: "core_nodes.generators:Circle",
        input: "color",
        merged_into: MergedInto::Color,
        parts: &[
            MergedPart::FedBy {
                old_input: "red",
                default: 1.0,
            },
            MergedPart::FedBy {
                old_input: "green",
                default: 1.0,
            },
            MergedPart::FedBy {
                old_input: "blue",
                default: 1.0,
            },
            MergedPart::Fixed(1.0),
        ],
    },
    // Rectangle's centre and colour.
    MergedInput {
        node_type: "core_nodes.generators:Rectangle",
        input: "center",
        merged_into: MergedInto::Vector,
        parts: &[
            MergedPart::FedBy {
                old_input: "center_x",
                default: 0.5,
            },
            MergedPart::FedBy {
                old_input: "center_y",
                default: 0.5,
            },
        ],
    },
    MergedInput {
        node_type: "core_nodes.generators:Rectangle",
        input: "color",
        merged_into: MergedInto::Color,
        parts: &[
            MergedPart::FedBy {
                old_input: "red",
                default: 1.0,
            },
            MergedPart::FedBy {
                old_input: "green",
                default: 1.0,
            },
            MergedPart::FedBy {
                old_input: "blue",
                default: 1.0,
            },
            MergedPart::Fixed(1.0),
        ],
    },
    // Transform's move.
    MergedInput {
        node_type: "core_nodes.transform:Transform",
        input: "move",
        merged_into: MergedInto::Vector,
        parts: &[
            MergedPart::FedBy {
                old_input: "move_x",
                default: 0.0,
            },
            MergedPart::FedBy {
                old_input: "move_y",
                default: 0.0,
            },
        ],
    },
    // Tile's scroll.
    MergedInput {
        node_type: "core_nodes.transform:Tile",
        input: "scroll",
        merged_into: MergedInto::Vector,
        parts: &[
            MergedPart::FedBy {
                old_input: "scroll_x",
                default: 0.0,
            },
            MergedPart::FedBy {
                old_input: "scroll_y",
                default: 0.0,
            },
        ],
    },
];
