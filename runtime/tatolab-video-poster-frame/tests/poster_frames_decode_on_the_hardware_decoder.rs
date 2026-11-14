// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The helper decodes on the machine's hardware decoder: the checked-in clips' first access
//! units decode to their 320×180 picture, and an MP4 ffmpeg writes becomes a 512 px poster.
//! Set `TATOLAB_VIDEO_POSTER_FRAME_EVIDENCE_DIRECTORY` to keep each poster as a PNG.

use std::path::{Path, PathBuf};
use std::process::Command;

use streamlib_asset_media_facts::{RgbaPictureFromThePosterFrameHelper, VideoPosterFrameHelper};

const CLIP_PIXEL_WIDTH: u32 = 320;
const CLIP_PIXEL_HEIGHT: u32 = 180;

fn the_helper() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_tatolab-video-poster-frame"))
}

fn first_access_unit_of_the_checked_in_clip(clip_file_name: &str) -> Vec<u8> {
    let clip = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../streamlib-engine/tests/fixtures/cross_floor_codec_clips")
            .join(clip_file_name),
    )
    .expect("the checked-in clip reads");
    let (length, rest) = clip
        .split_first_chunk::<4>()
        .expect("the clip holds an access unit");
    rest[..u32::from_be_bytes(*length) as usize].to_vec()
}

fn decoded_by_the_helper(
    codec: &str,
    annex_b_access_unit: &[u8],
) -> RgbaPictureFromThePosterFrameHelper {
    use std::io::Write;
    let mut helper = Command::new(the_helper())
        .args(["--codec", codec])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the helper starts");
    helper
        .stdin
        .take()
        .unwrap()
        .write_all(annex_b_access_unit)
        .unwrap();
    let finished = helper.wait_with_output().expect("the helper finishes");
    assert!(
        finished.status.success(),
        "the helper refused: {}",
        String::from_utf8_lossy(&finished.stderr)
    );
    RgbaPictureFromThePosterFrameHelper::read_from(&finished.stdout).expect("a picture")
}

fn keep_as_evidence(file_name: &str, png_bytes: &[u8]) {
    if let Some(directory) = std::env::var_os("TATOLAB_VIDEO_POSTER_FRAME_EVIDENCE_DIRECTORY") {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(Path::new(&directory).join(file_name), png_bytes).unwrap();
    }
}

fn assert_the_clip_decodes(codec: &str, clip_file_name: &str) {
    let picture = decoded_by_the_helper(
        codec,
        &first_access_unit_of_the_checked_in_clip(clip_file_name),
    );
    assert_eq!(
        (picture.pixel_width, picture.pixel_height),
        (CLIP_PIXEL_WIDTH, CLIP_PIXEL_HEIGHT)
    );
    let (pixels, _) = picture.rgba_bytes.as_chunks::<4>();
    assert!(
        pixels.iter().any(|pixel| *pixel != pixels[0]),
        "the decoded picture is one flat colour"
    );
    let mut png_bytes = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_raw(
        picture.pixel_width,
        picture.pixel_height,
        picture.rgba_bytes,
    )
    .unwrap()
    .write_to(&mut png_bytes, image::ImageFormat::Png)
    .unwrap();
    keep_as_evidence(
        &format!("{clip_file_name}.first_picture.png"),
        png_bytes.get_ref(),
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — needs a hardware decoder; set --features hardware-tests"
)]
#[test]
fn the_h264_clips_first_access_unit_decodes_to_its_picture() {
    assert_the_clip_decodes("h264", "videotoolbox_h264_320x180.access_units");
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — needs a hardware decoder; set --features hardware-tests"
)]
#[test]
fn the_h265_clips_first_access_unit_decodes_to_its_picture() {
    assert_the_clip_decodes("h265", "videotoolbox_h265_320x180.access_units");
}

fn assert_an_mp4_ffmpeg_writes_becomes_a_poster(encoder_arguments: &[&str], file_name: &str) {
    let directory = tempfile::tempdir().unwrap();
    let mp4 = directory.path().join(file_name);
    let written = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg("testsrc2=size=1280x720:rate=30")
        .args(["-t", "2", "-pix_fmt", "yuv420p"])
        .args(encoder_arguments)
        .arg(&mp4)
        .status()
        .expect("ffmpeg runs; the rig carries it");
    assert!(written.success(), "ffmpeg wrote no {file_name}");
    let poster = VideoPosterFrameHelper::at(the_helper())
        .poster_frame_of_a_video_file(&mp4)
        .expect("a poster frame");
    assert_eq!((poster.pixel_width, poster.pixel_height), (512, 288));
    keep_as_evidence(&format!("{file_name}.poster.png"), &poster.png_bytes);
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — needs a hardware decoder and ffmpeg; set --features hardware-tests"
)]
#[test]
fn an_h264_mp4_becomes_a_512_pixel_poster() {
    assert_an_mp4_ffmpeg_writes_becomes_a_poster(&["-c:v", "libx264", "-bf", "2"], "h264.mp4");
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — needs a hardware decoder and ffmpeg; set --features hardware-tests"
)]
#[test]
fn an_h265_mp4_becomes_a_512_pixel_poster() {
    assert_an_mp4_ffmpeg_writes_becomes_a_poster(
        &[
            "-c:v",
            "libx265",
            "-tag:v",
            "hvc1",
            "-x265-params",
            "log-level=error",
        ],
        "h265.mp4",
    );
}
