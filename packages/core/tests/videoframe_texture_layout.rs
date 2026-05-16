// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `VideoFrame.texture_layout` is an optional `i32` sidecar that producers
//! may set to override the per-surface `current_image_layout` from
//! surface-share IPC. Lock the serialization shape: present when set,
//! absent when `None` (`skip_serializing_if = "Option::is_none"` keeps
//! backward compat with older consumers). Mentally revert the
//! `skip_serializing_if` in the generated `video_frame.rs` and the
//! field-absent assertions start emitting `"texture_layout":null`.

use streamlib_core_schema_tests::_generated_::VideoFrame;

#[test]
fn videoframe_texture_layout_serialization_round_trip() {
    let with_layout = VideoFrame {
        surface_id: "s".to_string(),
        width: 8,
        height: 8,
        timestamp_ns: "0".to_string(),
        frame_index: "1".to_string(),
        fps: None,
        // SHADER_READ_ONLY_OPTIMAL = 5 per Vulkan spec.
        texture_layout: Some(5),
        color_info: None,
        mastering_display: None,
        content_light: None,
    };
    let json = serde_json::to_value(&with_layout).expect("serialize");
    assert_eq!(
        json.get("texture_layout").and_then(|v| v.as_i64()),
        Some(5),
        "set texture_layout must round-trip"
    );

    let absent = VideoFrame {
        surface_id: "s".to_string(),
        width: 8,
        height: 8,
        timestamp_ns: "0".to_string(),
        frame_index: "1".to_string(),
        fps: None,
        texture_layout: None,
        color_info: None,
        mastering_display: None,
        content_light: None,
    };
    let json_absent = serde_json::to_value(&absent).expect("serialize");
    assert!(
        json_absent.get("texture_layout").is_none(),
        "None texture_layout must be absent from the wire (back-compat with older consumers)"
    );

    // Round-trip through a JSON string: deserialize must recover the
    // field both directions (set → Some, absent → None).
    let serialized = serde_json::to_string(&with_layout).unwrap();
    let parsed: VideoFrame = serde_json::from_str(&serialized).unwrap();
    assert_eq!(parsed.texture_layout, Some(5));
    let serialized_absent = serde_json::to_string(&absent).unwrap();
    let parsed_absent: VideoFrame = serde_json::from_str(&serialized_absent).unwrap();
    assert_eq!(parsed_absent.texture_layout, None);
}
