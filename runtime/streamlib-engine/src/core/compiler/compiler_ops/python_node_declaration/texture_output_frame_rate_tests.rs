// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The frame rate a described node's Texture output states, stated or read from one of its
//! node's settings.

use super::tests::{declared_pin_of, described_node_type_with_input_ports, read, refusal_of};
use crate::core::descriptors::{ImageOutputPortFrameRate, ImageStreamFrameRate};

/// A described node type whose Texture output `video` states its frame rate as `frame_rate`,
/// beside a setting `frames_per_second`.
fn a_described_node_type_whose_texture_output_states(
    frame_rate: serde_json::Value,
) -> serde_json::Value {
    let mut described = described_node_type_with_input_ports(serde_json::json!([{
        "name": "frames_per_second",
        "family": {"family": "channels"},
        "description": "",
        "input_mode": {"mode": "setting_only"},
        "pin_declaration": {"pin_type": {"kind": "is", "pin_type": {"type": "integer"}}, "flow": "value"},
    }]));
    described["output_ports"] = serde_json::json!([{
        "name": "video",
        "family": {"family": "image"},
        "description": "",
        "pin_declaration": {
            "pin_type": {"kind": "is", "pin_type": {"type": "texture"}},
            "flow": "stream",
            "annotations": {"frame_rate": frame_rate},
        },
    }]);
    described
}

/// Mental-revert: reading no setting for a frame rate leaves a stream whose rate names no
/// setting reading none, with nothing to say why.
#[test]
fn a_texture_outputs_frame_rate_reads_stated_or_from_a_setting_the_node_declares() {
    let stated = read(&a_described_node_type_whose_texture_output_states(
        serde_json::json!({"stated": {"numerator": 30000, "denominator": 1001}}),
    ))
    .expect("a stated frame rate reads");
    assert_eq!(
        declared_pin_of(&stated, "video")
            .expect("declares a pin")
            .annotations
            .frame_rate,
        Some(ImageOutputPortFrameRate::Stated(ImageStreamFrameRate {
            numerator: 30_000,
            denominator: 1_001
        }))
    );
    read(&a_described_node_type_whose_texture_output_states(
        serde_json::json!({"from_config": {"config_key": "frames_per_second"}}),
    ))
    .expect("a frame rate read from a setting the node declares reads");

    let refusal = refusal_of(&a_described_node_type_whose_texture_output_states(
        serde_json::json!({"from_config": {"config_key": "fps"}}),
    ));
    assert!(
        refusal.contains(
            "output \"video\" runs at the frame rate its node's setting \"fps\" holds, and the \
             node declares no setting \"fps\""
        ),
        "{refusal}"
    );
}
