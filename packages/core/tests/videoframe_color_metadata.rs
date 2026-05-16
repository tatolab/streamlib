// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `VideoFrame.{color_info, mastering_display, content_light}` are
//! optional sidecar fields that producers populate from V4L2 / VUI /
//! EDID / HDR metadata. Lock the serialization shape: present when
//! set (with all sub-fields round-tripping), absent when `None`.
//! Mentally revert the `skip_serializing_if = "Option::is_none"` on
//! either field in the generated `_generated_/tatolab__core/video_frame.rs`
//! and the absent assertions start failing.

use streamlib_core_schema_tests::_generated_::tatolab__core::color_info::{
    Matrix, Primaries, Range, Transfer,
};
use streamlib_core_schema_tests::_generated_::{
    ColorInfo, ContentLight, MasteringDisplay, VideoFrame,
};

#[test]
fn videoframe_color_metadata_round_trip() {
    let with_color = VideoFrame {
        surface_id: "s".to_string(),
        width: 1920,
        height: 1080,
        timestamp_ns: "0".to_string(),
        frame_index: "0".to_string(),
        fps: None,
        texture_layout: None,
        color_info: Some(ColorInfo {
            primaries: Some(Primaries::Bt2020),
            transfer: Some(Transfer::Smpte2084),
            matrix: Some(Matrix::Bt2020Ncl),
            range: Some(Range::Limited),
        }),
        mastering_display: Some(MasteringDisplay {
            // BT.2020 primaries in 1/50000 increments — round-trip
            // proof, not a real mastering display.
            display_primaries_r_x: 35400,
            display_primaries_r_y: 14600,
            display_primaries_g_x: 8500,
            display_primaries_g_y: 39850,
            display_primaries_b_x: 6550,
            display_primaries_b_y: 2300,
            white_point_x: 15635,
            white_point_y: 16450,
            min_luminance: 1, // 0.0001 cd/m^2
            max_luminance: 10_000_000, // 1000 cd/m^2
        }),
        content_light: Some(ContentLight {
            max_cll: 1000,
            max_fall: 400,
        }),
    };
    let json = serde_json::to_value(&with_color).expect("serialize");
    assert!(json.get("color_info").is_some());
    assert!(json.get("mastering_display").is_some());
    assert!(json.get("content_light").is_some());
    // Enums round-trip as snake_case strings (H.273 alignment).
    assert_eq!(
        json.pointer("/color_info/transfer").and_then(|v| v.as_str()),
        Some("smpte2084"),
        "transfer enum serializes as the snake_case discriminant"
    );

    let serialized = serde_json::to_string(&with_color).unwrap();
    let parsed: VideoFrame = serde_json::from_str(&serialized).unwrap();
    let parsed_color = parsed.color_info.expect("color_info round-trips");
    assert_eq!(parsed_color.primaries, Some(Primaries::Bt2020));
    assert_eq!(parsed_color.transfer, Some(Transfer::Smpte2084));
    assert_eq!(parsed_color.matrix, Some(Matrix::Bt2020Ncl));
    assert_eq!(parsed_color.range, Some(Range::Limited));

    // ColorInfo with all axes None — wire-format-correct
    // representation of "I have a ColorInfo record but every axis is
    // unspecified." Lock the per-axis skip-on-None behavior;
    // mentally revert `optionalProperties` in the schema and the
    // JSON gains four `null` axis fields.
    let info_all_none = ColorInfo::default();
    let json_info_none = serde_json::to_value(&info_all_none).expect("serialize");
    assert_eq!(json_info_none, serde_json::json!({}));

    // Absent sidecar fields disappear from the wire entirely.
    let without_color = VideoFrame {
        surface_id: "s".to_string(),
        width: 1920,
        height: 1080,
        timestamp_ns: "0".to_string(),
        frame_index: "0".to_string(),
        fps: None,
        texture_layout: None,
        color_info: None,
        mastering_display: None,
        content_light: None,
    };
    let json_absent = serde_json::to_value(&without_color).expect("serialize");
    assert!(json_absent.get("color_info").is_none());
    assert!(json_absent.get("mastering_display").is_none());
    assert!(json_absent.get("content_light").is_none());
}
