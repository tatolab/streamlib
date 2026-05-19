// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Lock the msgpack wire shape for binary-bearing schema fields against
//! regression. The jtd-codegen pipeline emits `#[serde(with = "serde_bytes")]`
//! on Rust fields backed by JTD `elements: type: uint8` so `rmp_serde`
//! writes the payload as msgpack `bin` (1× wire footprint) instead of as
//! an array of integers (~1.5× footprint for bytes ≥ 128, since each
//! such value takes 2 wire bytes as a typed int).
//!
//! Mentally revert the attribute on `EncodedVideoFrame.data` in
//! `libs/streamlib-jtd-codegen/src/lib.rs::post_process_rust` and these
//! assertions fail (the array-tag check fires, the bin-tag check misses,
//! and the wire-size budget blows out by ~100 bytes).

use streamlib_core_schema_tests::_generated_::{EncodedAudioFrame, EncodedVideoFrame};

/// msgpack `bin 8` tag — payload length < 256, written as `0xc4 LEN <bytes>`.
const MSGPACK_BIN_8: u8 = 0xc4;
/// msgpack `array 16` tag — would be `0xdc LEN_HI LEN_LO <values...>` if a
/// `Vec<u8>` were serialized as an array (the regression we're guarding).
const MSGPACK_ARRAY_16: u8 = 0xdc;

#[test]
fn encoded_video_frame_data_serializes_as_msgpack_bin() {
    // 100 bytes of 0xff (each byte ≥ 128 so the without-serde_bytes shape
    // pays its worst case of 2 wire bytes per element).
    //
    //   With serde_bytes: 0xc4 0x64 <100 × 0xff> = 102 bytes of payload.
    //   Without:          0xdc 0x00 0x64 <100 × {0xcc 0xff}> = 203 bytes.
    let frame = EncodedVideoFrame {
        data: vec![0xff_u8; 100],
        timestamp_ns: "0".to_string(),
        is_keyframe: true,
        frame_number: "0".to_string(),
        fps: None,
        color_info: None,
        mastering_display: None,
        content_light: None,
    };

    let wire = rmp_serde::to_vec_named(&frame).expect("rmp_serde::to_vec_named");

    let bin_tag_pos = wire
        .windows(2)
        .position(|w| w[0] == MSGPACK_BIN_8 && w[1] == 100);
    assert!(
        bin_tag_pos.is_some(),
        "expected `bin 8` tag (0xc4) followed by length 100 in wire output; \
         got len={} wire={:02x?}",
        wire.len(),
        wire
    );

    let array_tag_present = wire.iter().any(|&b| b == MSGPACK_ARRAY_16);
    assert!(
        !array_tag_present,
        "wire output contains `array 16` tag (0xdc) — this means the \
         `data` field serialized as an integer array instead of msgpack \
         `bin`. Did `#[serde(with = \"serde_bytes\")]` get dropped from \
         the codegen output? wire={:02x?}",
        wire
    );

    // Total wire size: the bin path puts the entire payload at 102 bytes
    // plus the map overhead for the surrounding `EncodedVideoFrame` fields.
    // The array path would be ~100 bytes larger. A generous 160-byte cap
    // proves the bin path won without depending on exact field-count math.
    assert!(
        wire.len() < 160,
        "wire output too large ({} bytes) — the bin path produces ~140; the \
         array path produces ~240. Difference is the regression signal.",
        wire.len()
    );
}

#[test]
fn encoded_video_frame_zero_payload_serializes_as_empty_bin() {
    // Empty payload: bin 8 with length 0 (`0xc4 0x00`) — different code
    // path than the non-empty case (length encoding still uses bin 8 for
    // anything < 256 bytes).
    let frame = EncodedVideoFrame {
        data: Vec::new(),
        timestamp_ns: "0".to_string(),
        is_keyframe: false,
        frame_number: "0".to_string(),
        fps: None,
        color_info: None,
        mastering_display: None,
        content_light: None,
    };

    let wire = rmp_serde::to_vec_named(&frame).expect("rmp_serde::to_vec_named");

    let bin_tag_pos = wire
        .windows(2)
        .position(|w| w[0] == MSGPACK_BIN_8 && w[1] == 0);
    assert!(
        bin_tag_pos.is_some(),
        "expected `bin 8` tag with length 0 for empty `data`; wire={:02x?}",
        wire
    );
}

#[test]
fn encoded_video_frame_roundtrips_through_rmp_serde() {
    let frame = EncodedVideoFrame {
        data: (0u8..=255u8).collect(),
        timestamp_ns: "12345".to_string(),
        is_keyframe: true,
        frame_number: "42".to_string(),
        fps: Some(30),
        color_info: None,
        mastering_display: None,
        content_light: None,
    };

    let wire = rmp_serde::to_vec_named(&frame).expect("rmp_serde::to_vec_named");
    let decoded: EncodedVideoFrame =
        rmp_serde::from_slice(&wire).expect("rmp_serde::from_slice");

    assert_eq!(decoded, frame, "round-trip must be exact");
}

#[test]
fn encoded_audio_frame_data_serializes_as_msgpack_bin() {
    // Companion lock for `EncodedAudioFrame.data` — the codegen path that
    // produces this attribute is the same as `EncodedVideoFrame`'s, but a
    // per-schema test catches a regression that skipped one specific
    // generated file (e.g., a schema-discovery bug in the build.rs).
    let frame = EncodedAudioFrame {
        data: vec![0xff_u8; 100],
        timestamp_ns: "0".to_string(),
        sample_count: 960,
    };
    let wire = rmp_serde::to_vec_named(&frame).expect("rmp_serde::to_vec_named");

    let bin_tag_pos = wire
        .windows(2)
        .position(|w| w[0] == MSGPACK_BIN_8 && w[1] == 100);
    assert!(
        bin_tag_pos.is_some(),
        "EncodedAudioFrame.data expected as `bin 8` (0xc4) with length 100; \
         wire={:02x?}",
        wire
    );

    let array_tag_present = wire.iter().any(|&b| b == MSGPACK_ARRAY_16);
    assert!(
        !array_tag_present,
        "wire contains `array 16` (0xdc) — codegen attribute regressed on \
         EncodedAudioFrame.data; wire={:02x?}",
        wire
    );
}
