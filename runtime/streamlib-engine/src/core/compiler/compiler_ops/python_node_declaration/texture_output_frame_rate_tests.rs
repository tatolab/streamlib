// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The frame rate a described node's Texture output states, stated or read from one of its
//! node's settings.

use super::tests::{declared_pin_of, described_node_type_with_input_ports, read, refusal_of};
use crate::core::descriptors::{ImageOutputPortFrameRate, ImageStreamFrameRate};

/// A described node type whose Texture output `video` states its frame rate as `frame_rate`,
/// beside an Integer setting `frames_per_second`, a String setting `title`, a Bundle setting
/// `rate` of an Integer `numerator` and `denominator`, and a Bundle setting `look` of a String.
fn a_described_node_type_whose_texture_output_states(
    frame_rate: serde_json::Value,
) -> serde_json::Value {
    let mut described = described_node_type_with_input_ports(serde_json::json!([{
        "name": "frames_per_second",
        "description": "",
        "input_mode": {"mode": "setting_only"},
        "pin_declaration": {"pin_type": {"kind": "is", "pin_type": {"type": "integer"}}, "flow": "value"},
    }, {
        "name": "title",
        "description": "",
        "input_mode": {"mode": "setting_only"},
        "pin_declaration": {"pin_type": {"kind": "is", "pin_type": {"type": "string"}}, "flow": "value"},
    }, {
        "name": "rate",
        "description": "",
        "input_mode": {"mode": "setting_only"},
        "pin_declaration": {"pin_type": {"kind": "is", "pin_type": {"type": "bundle", "members": [
            {"name": "numerator", "member_type": {"type": "integer"}},
            {"name": "denominator", "member_type": {"type": "integer"}},
        ]}}, "flow": "value"},
    }, {
        "name": "look",
        "description": "",
        "input_mode": {"mode": "setting_only"},
        "pin_declaration": {"pin_type": {"kind": "is", "pin_type": {"type": "bundle", "members": [
            {"name": "label", "member_type": {"type": "string"}},
        ]}}, "flow": "value"},
    }]));
    described["output_ports"] = serde_json::json!([{
        "name": "video",
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
    read(&a_described_node_type_whose_texture_output_states(
        serde_json::json!({"from_config": {"config_key": "rate"}}),
    ))
    .expect("a frame rate read from a Bundle of a numerator and a denominator reads");

    let refusal = refusal_of(&a_described_node_type_whose_texture_output_states(
        serde_json::json!({"from_config": {"config_key": "fps"}}),
    ));
    assert!(
        refusal.contains(
            "output \"video\" runs at the frame rate its node's setting \"fps\" holds, and the \
             node declares no setting \"fps\". To fix:"
        ),
        "{refusal}"
    );

    let refusal = refusal_of(&a_described_node_type_whose_texture_output_states(
        serde_json::json!({"from_config": {"config_key": "title"}}),
    ));
    assert!(
        refusal.contains(
            "output \"video\" runs at the frame rate its node's setting \"title\" holds, and \
             that setting is a String, which holds no frame rate. To fix: type \"title\" an \
             Integer of frames per second"
        ),
        "{refusal}"
    );

    let refusal = refusal_of(&a_described_node_type_whose_texture_output_states(
        serde_json::json!({"from_config": {"config_key": "look"}}),
    ));
    assert!(
        refusal.contains("setting \"look\" holds, and that setting is a Bundle"),
        "{refusal}"
    );
}

/// Mental-revert: checking only a node's own outputs lets a pin group's Texture output read its
/// frame rate from a setting no one declares, and its stream then has none.
#[test]
fn a_pin_groups_texture_output_reading_its_frame_rate_from_no_setting_is_refused() {
    let mut described = described_node_type_with_input_ports(serde_json::json!([]));
    described["pin_groups"] = serde_json::json!([{
        "name": "screens",
        "description": "",
        "inputs": [],
        "outputs": [{
            "name": "picture",
            "description": "",
            "pin_declaration": {
                "pin_type": {"kind": "is", "pin_type": {"type": "texture"}},
                "flow": "stream",
                "annotations": {"frame_rate": {"from_config": {"config_key": "fps"}}},
            },
        }],
    }]);

    let refusal = refusal_of(&described);

    assert!(
        refusal.contains("output \"picture\" runs at the frame rate its node's setting \"fps\""),
        "{refusal}"
    );
}
