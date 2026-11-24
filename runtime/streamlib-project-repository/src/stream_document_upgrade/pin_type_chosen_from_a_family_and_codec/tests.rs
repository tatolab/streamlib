// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::{Value, json};

use crate::stream_document_upgrade::upgrade_a_node_in_place;

fn the_receive_saved_with(config: Value) -> Value {
    let mut node =
        json!({"name": "receive", "type": "tatolab.moq:MoqTrackReceive", "config": config});
    upgrade_a_node_in_place(&mut node);
    node["input_values"].clone()
}

/// Mental-revert: without the row, `family` and `codec` stay values of inputs the node no
/// longer declares, and `content_type` chooses its default, a `MoqRecord`, for every track.
#[test]
fn a_saved_family_and_codec_become_the_type_the_receives_choice_holds() {
    let moq_record = json!({"type": "named", "named_pin_type": "tatolab.moq:MoqRecord"});
    for (config, chosen) in [
        (json!({"family": "records"}), moq_record.clone()),
        (
            json!({"family": "records", "codec": "h264"}),
            moq_record.clone(),
        ),
        (json!({"codec": "h264"}), moq_record),
        (
            json!({"family": "packets", "codec": "h264"}),
            json!({"type": "encoded_video", "codecs": ["h264"]}),
        ),
        (
            json!({"family": "packets", "codec": "opus"}),
            json!({"type": "encoded_audio", "codecs": ["opus"]}),
        ),
    ] {
        let mut expected = json!({"track": "camera"});
        expected["content_type"] = chosen;
        let mut saved = config.clone();
        saved["track"] = json!("camera");
        assert_eq!(the_receive_saved_with(saved), expected, "saved {config}");
    }
}

/// A receive saved reading Packets of no codec, or of one it no longer reads, opens as written,
/// so it is refused naming `family` and `codec` rather than read as a type it never carried.
#[test]
fn a_saved_packets_family_of_no_codec_or_one_the_choice_does_not_hold_is_left_as_written() {
    for config in [
        json!({"family": "packets"}),
        json!({"family": "packets", "codec": "aac"}),
        json!({"family": "packets", "codec": "av1"}),
    ] {
        assert_eq!(the_receive_saved_with(config.clone()), config);
    }
}

#[test]
fn a_receive_holding_its_choice_keeps_it_and_one_holding_neither_is_left_as_written() {
    let h265 = json!({"type": "encoded_video", "codecs": ["h265"]});
    assert_eq!(
        the_receive_saved_with(json!({"family": "packets", "codec": "h264", "content_type": h265})),
        json!({"content_type": h265})
    );
    assert_eq!(
        the_receive_saved_with(json!({"track": "camera"})),
        json!({"track": "camera"})
    );
}
