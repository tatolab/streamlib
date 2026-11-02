// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The decoder on a device a codec module owns: the checked-in clips a Mac's
//! VideoToolbox encoder wrote decode, one access unit at a time, to one RGBA
//! picture each at the clip's conformance extent.

use std::path::{Path, PathBuf};

use crate::encode::Codec;
use crate::hardware_test_codec_device::owned_video_decode_device_on_the_first_gpu;
use crate::{SimpleDecoder, SimpleDecoderConfig};

const CLIP_WIDTH: u32 = 320;
const CLIP_HEIGHT: u32 = 180;

fn checked_in_clip_path(file_name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../streamlib-engine/tests/fixtures/cross_floor_codec_clips")
        .join(file_name)
}

/// Each access unit is stored as its byte count, a big-endian `u32`, then
/// its bytes.
fn read_access_unit_clip(path: &Path) -> Vec<Vec<u8>> {
    let clip = std::fs::read(path).expect("the checked-in clip reads");
    let mut access_units = Vec::new();
    let mut remaining = clip.as_slice();
    while let Some((length, rest)) = remaining.split_first_chunk::<4>() {
        let length = u32::from_be_bytes(*length) as usize;
        let (access_unit, rest) = rest.split_at(length);
        access_units.push(access_unit.to_vec());
        remaining = rest;
    }
    access_units
}

fn decode_the_clip_on_an_owned_device(codec: Codec, clip_file_name: &str) {
    let Some(owned_decode_device) = owned_video_decode_device_on_the_first_gpu() else {
        return;
    };
    let mut decoder = SimpleDecoder::new(
        owned_decode_device,
        SimpleDecoderConfig {
            codec,
            rgba_output: true,
            ..SimpleDecoderConfig::default()
        },
    )
    .expect("a decoder on the owned device");
    let access_units = read_access_unit_clip(&checked_in_clip_path(clip_file_name));
    assert!(
        !access_units.is_empty(),
        "{clip_file_name} holds access units"
    );
    let mut pictures = Vec::new();
    for access_unit in &access_units {
        pictures.extend(decoder.feed(access_unit).expect("the access unit decodes"));
    }
    assert_eq!(
        pictures.len(),
        access_units.len(),
        "one picture per access unit of {clip_file_name}"
    );
    for picture in &pictures {
        assert!(picture.is_rgba);
        assert_eq!((picture.width, picture.height), (CLIP_WIDTH, CLIP_HEIGHT));
        assert_eq!(
            picture.data.len(),
            (CLIP_WIDTH * CLIP_HEIGHT * 4) as usize,
            "the RGBA picture is the conformance window, packed"
        );
    }
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — needs Vulkan Video decode; set --features hardware-tests"
)]
#[test]
fn an_owned_device_decodes_the_h264_clip_videotoolbox_encoded() {
    decode_the_clip_on_an_owned_device(Codec::H264, "videotoolbox_h264_320x180.access_units");
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — needs Vulkan Video decode; set --features hardware-tests"
)]
#[test]
fn an_owned_device_decodes_the_h265_clip_videotoolbox_encoded() {
    decode_the_clip_on_an_owned_device(Codec::H265, "videotoolbox_h265_320x180.access_units");
}
