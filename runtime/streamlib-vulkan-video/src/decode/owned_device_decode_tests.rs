// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The decoder on a device a codec module owns: the checked-in clips a Mac's
//! VideoToolbox encoder wrote decode, one access unit at a time, to one RGBA
//! picture each at the clip's conformance extent, and to exported NV12
//! DMA-BUFs whose mapped bytes are the pictures read back.

use std::path::{Path, PathBuf};

use crate::encode::Codec;
use crate::hardware_test_codec_device::owned_video_decode_device_on_the_first_gpu;
use crate::{
    DecodedPictureDelivery, ExportedNv12DmaBufPicture, SimpleDecodedFrame, SimpleDecoder,
    SimpleDecoderConfig,
};

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

/// Every picture the clip decodes to on the first GPU's owned decode device,
/// handed back by `decoded_picture_delivery`, each passed to `inspect` before
/// the next access unit is fed; `None` with no such device.
fn decode_the_clip_on_an_owned_device_inspecting_each_picture(
    codec: Codec,
    clip_file_name: &str,
    decoded_picture_delivery: DecodedPictureDelivery,
    mut inspect: impl FnMut(&SimpleDecodedFrame),
) -> Option<Vec<SimpleDecodedFrame>> {
    let owned_decode_device = owned_video_decode_device_on_the_first_gpu()?;
    let mut decoder = SimpleDecoder::new(
        owned_decode_device,
        SimpleDecoderConfig {
            codec,
            decoded_picture_delivery,
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
        for picture in decoder.feed(access_unit).expect("the access unit decodes") {
            inspect(&picture);
            pictures.push(picture);
        }
    }
    assert_eq!(
        pictures.len(),
        access_units.len(),
        "one picture per access unit of {clip_file_name}"
    );
    Some(pictures)
}

fn decode_the_clip_on_an_owned_device(codec: Codec, clip_file_name: &str) {
    let Some(pictures) = decode_the_clip_on_an_owned_device_inspecting_each_picture(
        codec,
        clip_file_name,
        DecodedPictureDelivery::ReadBackRgba,
        |_| {},
    ) else {
        return;
    };
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

/// The display window's luma rows an exported picture holds, read through a
/// CPU mapping of its DMA-BUF — the way an importer that cannot import the
/// buffer into its own device reads it.
fn luma_rows_mapped_from_the_exported_dma_buf(
    exported: &ExportedNv12DmaBufPicture,
    width: u32,
    height: u32,
) -> Vec<u8> {
    use std::os::fd::AsRawFd as _;
    let byte_count = exported.allocation_byte_size as usize;
    // SAFETY: a read-only shared mapping of the whole allocation, unmapped
    // below before anything else touches the fd.
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            byte_count,
            libc::PROT_READ,
            libc::MAP_SHARED,
            exported.dma_buf_fd.as_raw_fd(),
            0,
        )
    };
    assert_ne!(
        mapping,
        libc::MAP_FAILED,
        "the exported DMA-BUF maps for CPU reads: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: the mapping spans `byte_count` readable bytes until munmap.
    let mapped = unsafe { std::slice::from_raw_parts(mapping.cast::<u8>(), byte_count) };
    let mut luma_rows = Vec::with_capacity((width * height) as usize);
    for row in 0..u64::from(height) {
        let row_start =
            (exported.plane_offsets_bytes[0] + row * exported.plane_strides_bytes[0]) as usize;
        luma_rows.extend_from_slice(&mapped[row_start..row_start + width as usize]);
    }
    // SAFETY: the mapping made above, no longer borrowed.
    unsafe { libc::munmap(mapping, byte_count) };
    luma_rows
}

fn the_exported_pictures_match_the_read_back_ones(codec: Codec, clip_file_name: &str) {
    let mut exported_luma_rows = Vec::new();
    let Some(exported_pictures) = decode_the_clip_on_an_owned_device_inspecting_each_picture(
        codec,
        clip_file_name,
        DecodedPictureDelivery::Nv12InAnExportedDmaBuf,
        |picture| {
            let exported = picture
                .exported_nv12
                .as_ref()
                .expect("an exported picture names its DMA-BUF");
            assert!(picture.data.is_empty(), "nothing is read back");
            assert_eq!((picture.width, picture.height), (CLIP_WIDTH, CLIP_HEIGHT));
            exported_luma_rows.push(luma_rows_mapped_from_the_exported_dma_buf(
                exported,
                picture.width,
                picture.height,
            ));
        },
    ) else {
        return;
    };
    let read_back_pictures = decode_the_clip_on_an_owned_device_inspecting_each_picture(
        codec,
        clip_file_name,
        DecodedPictureDelivery::ReadBackNv12,
        |_| {},
    )
    .expect("the device decoded the clip a moment ago");
    let ring_slots_used: std::collections::BTreeSet<usize> = exported_pictures
        .iter()
        .filter_map(|picture| picture.exported_nv12.as_ref())
        .map(|exported| exported.ring_slot_index)
        .collect();
    assert!(
        ring_slots_used.len() > 1,
        "consecutive pictures rotate through the ring"
    );
    for (picture_index, (read_back, exported_luma)) in read_back_pictures
        .iter()
        .zip(&exported_luma_rows)
        .enumerate()
    {
        let coded_width = read_back.width as usize;
        let read_back_luma: Vec<u8> = (0..CLIP_HEIGHT as usize)
            .flat_map(|row| {
                read_back.data[row * coded_width..row * coded_width + CLIP_WIDTH as usize]
                    .iter()
                    .copied()
            })
            .collect();
        assert!(
            read_back_luma == *exported_luma,
            "picture {picture_index}'s exported luma matches the luma read back"
        );
    }
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — needs Vulkan Video decode; set --features hardware-tests"
)]
#[test]
fn the_h264_clip_decodes_into_exported_dma_bufs_holding_the_read_back_pictures() {
    the_exported_pictures_match_the_read_back_ones(
        Codec::H264,
        "videotoolbox_h264_320x180.access_units",
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — needs Vulkan Video decode; set --features hardware-tests"
)]
#[test]
fn the_h265_clip_decodes_into_exported_dma_bufs_holding_the_read_back_pictures() {
    the_exported_pictures_match_the_read_back_ones(
        Codec::H265,
        "videotoolbox_h265_320x180.access_units",
    );
}

fn pictures_held_across_the_whole_clip_keep_their_own_buffers(codec: Codec, clip_file_name: &str) {
    let Some(exported_pictures) = decode_the_clip_on_an_owned_device_inspecting_each_picture(
        codec,
        clip_file_name,
        DecodedPictureDelivery::Nv12InAnExportedDmaBuf,
        |_| {},
    ) else {
        return;
    };
    let read_back_pictures = decode_the_clip_on_an_owned_device_inspecting_each_picture(
        codec,
        clip_file_name,
        DecodedPictureDelivery::ReadBackNv12,
        |_| {},
    )
    .expect("the device decoded the clip a moment ago");
    let ring_slots_used: std::collections::BTreeSet<usize> = exported_pictures
        .iter()
        .filter_map(|picture| picture.exported_nv12.as_ref())
        .map(|exported| exported.ring_slot_index)
        .collect();
    assert_eq!(
        ring_slots_used.len(),
        exported_pictures.len(),
        "no buffer a held picture names is written again"
    );
    for (picture_index, (exported, read_back)) in exported_pictures
        .iter()
        .zip(&read_back_pictures)
        .enumerate()
    {
        let exported_luma = luma_rows_mapped_from_the_exported_dma_buf(
            exported
                .exported_nv12
                .as_ref()
                .expect("an exported picture"),
            exported.width,
            exported.height,
        );
        let coded_width = read_back.width as usize;
        let read_back_luma: Vec<u8> = (0..CLIP_HEIGHT as usize)
            .flat_map(|row| {
                read_back.data[row * coded_width..row * coded_width + CLIP_WIDTH as usize]
                    .iter()
                    .copied()
            })
            .collect();
        assert!(
            read_back_luma == exported_luma,
            "picture {picture_index}, read after the clip ended, still holds its own luma"
        );
    }
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — needs Vulkan Video decode; set --features hardware-tests"
)]
#[test]
fn h264_pictures_held_across_the_whole_clip_keep_their_own_buffers() {
    pictures_held_across_the_whole_clip_keep_their_own_buffers(
        Codec::H264,
        "videotoolbox_h264_320x180.access_units",
    );
}
