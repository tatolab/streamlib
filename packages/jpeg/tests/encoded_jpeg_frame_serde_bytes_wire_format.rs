// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Wire-shape lock for `EncodedJpegFrame.data`. The codegen pipeline emits
//! `#[serde(with = "serde_bytes")]` on the field; `rmp_serde::to_vec_named`
//! must produce a msgpack `bin 8` tag (0xc4) rather than an array tag
//! (0xdc) so a `complex_pattern` JPEG at quality 95 fits inside iceoryx2's
//! 64 KiB per-slot default.

use streamlib_jpeg::EncodedJpegFrame;

const MSGPACK_BIN_8: u8 = 0xc4;
const MSGPACK_ARRAY_16: u8 = 0xdc;

#[test]
fn encoded_jpeg_frame_data_serializes_as_msgpack_bin() {
    let frame = EncodedJpegFrame {
        data: vec![0xff_u8; 100],
        timestamp_ns: "0".to_string(),
        frame_number: "0".to_string(),
        fps: None,
    };
    let wire = rmp_serde::to_vec_named(&frame).expect("rmp_serde::to_vec_named");

    let bin_tag_pos = wire
        .windows(2)
        .position(|w| w[0] == MSGPACK_BIN_8 && w[1] == 100);
    assert!(
        bin_tag_pos.is_some(),
        "EncodedJpegFrame.data expected as `bin 8` (0xc4) with length 100; \
         wire={:02x?}",
        wire
    );

    let array_tag_present = wire.iter().any(|&b| b == MSGPACK_ARRAY_16);
    assert!(
        !array_tag_present,
        "wire contains `array 16` (0xdc) — codegen attribute regressed on \
         EncodedJpegFrame.data; wire={:02x?}",
        wire
    );
}
