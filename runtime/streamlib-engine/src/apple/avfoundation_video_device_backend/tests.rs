// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn a_format(
    width: u32,
    height: u32,
    pixel_format: PixelFormat,
    highest_frames_per_second: f64,
) -> AvFoundationFormatCandidate {
    AvFoundationFormatCandidate {
        width,
        height,
        pixel_format,
        highest_frames_per_second,
        shortest_frame_duration: None,
    }
}

#[test]
fn the_largest_format_within_the_cap_is_chosen() {
    let formats = [
        a_format(640, 480, PixelFormat::Nv12VideoRange, 30.0),
        a_format(1920, 1080, PixelFormat::Nv12VideoRange, 30.0),
        a_format(3840, 2160, PixelFormat::Nv12VideoRange, 30.0),
        a_format(1280, 720, PixelFormat::Nv12VideoRange, 60.0),
    ];
    assert_eq!(
        index_of_the_format_to_capture_in(&formats, 1920, 1080),
        Some(1)
    );
}

#[test]
fn at_one_extent_the_faster_format_wins_and_then_video_range_biplanar() {
    let formats = [
        a_format(1280, 720, PixelFormat::Bgra32, 30.0),
        a_format(1280, 720, PixelFormat::Nv12FullRange, 30.0),
        a_format(1280, 720, PixelFormat::Nv12VideoRange, 30.0),
        a_format(1280, 720, PixelFormat::Yuyv422, 60.0),
    ];
    assert_eq!(
        index_of_the_format_to_capture_in(&formats, 1920, 1080),
        Some(3)
    );
    assert_eq!(
        index_of_the_format_to_capture_in(&formats[..3], 1920, 1080),
        Some(2)
    );
}

#[test]
fn a_device_with_nothing_within_the_cap_captures_in_its_smallest_format() {
    let formats = [
        a_format(3840, 2160, PixelFormat::Nv12VideoRange, 30.0),
        a_format(1920, 1080, PixelFormat::Nv12VideoRange, 30.0),
    ];
    assert_eq!(
        index_of_the_format_to_capture_in(&formats, 640, 480),
        Some(1)
    );
}

#[test]
fn a_device_that_offers_no_format_has_nothing_to_capture_in() {
    assert_eq!(index_of_the_format_to_capture_in(&[], 1920, 1080), None);
}

#[test]
fn biplanar_formats_are_delivered_as_they_are_and_anything_else_as_video_range() {
    assert_eq!(
        pixel_format_to_deliver_for(PixelFormat::Nv12FullRange),
        PixelFormat::Nv12FullRange
    );
    assert_eq!(
        pixel_format_to_deliver_for(PixelFormat::Nv12VideoRange),
        PixelFormat::Nv12VideoRange
    );
    assert_eq!(
        pixel_format_to_deliver_for(PixelFormat::Bgra32),
        PixelFormat::Nv12VideoRange
    );
}

#[test]
fn listing_cameras_succeeds_with_or_without_one_attached() {
    let devices = AvFoundationVideoDeviceBackend
        .list_capture_devices()
        .expect("enumeration must not error");
    let unique: std::collections::HashSet<&str> =
        devices.iter().map(|device| device.id.as_str()).collect();
    assert_eq!(
        unique.len(),
        devices.len(),
        "a camera is listed once: {devices:?}"
    );
}
