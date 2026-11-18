// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::conversion_node_config_refusals::*;

#[test]
fn channel_names_naming_none_or_one_twice_are_refused() {
    let none = refuse_channel_names_naming_none_or_one_twice("Node", &[])
        .unwrap_err()
        .to_string();
    let twice = refuse_channel_names_naming_none_or_one_twice(
        "Node",
        &["gain".into(), "pan".into(), "gain".into()],
    )
    .unwrap_err()
    .to_string();

    assert!(none.contains("names no channel"), "{none}");
    assert!(twice.contains("\"gain\" twice"), "{twice}");
    assert!(
        refuse_channel_names_naming_none_or_one_twice("Node", &["gain".into(), "pan".into()])
            .is_ok()
    );
}

#[test]
fn a_sample_rate_of_zero_is_refused() {
    let refusal = refuse_a_channels_output_config("Node", &["gain".into()], 0)
        .unwrap_err()
        .to_string();

    assert!(refusal.contains("`sample_rate` is 0"), "{refusal}");
}
