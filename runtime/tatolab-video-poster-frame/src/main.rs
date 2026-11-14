// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `tatolab-video-poster-frame --codec h264|h265`: reads one Annex-B access unit, a video's
//! first sync sample led by its parameter sets, on standard input; decodes it on the machine's
//! hardware decoder; and writes the picture to standard output as
//! [`RgbaPictureFromThePosterFrameHelper`] lays it out. A refusal is one line on standard error
//! and a non-zero exit.

#[cfg(target_os = "macos")]
mod apple;
#[cfg(target_os = "linux")]
mod linux;

use std::io::{Read, Write};
use std::process::ExitCode;

use streamlib_asset_media_facts::{
    PosterFrameVideoCodec, RgbaPictureFromThePosterFrameHelper,
    VIDEO_POSTER_FRAME_HELPER_CODEC_ARGUMENT,
};

/// The largest access unit read from standard input.
const LARGEST_ACCESS_UNIT_READ_BYTES: u64 = 64 * 1024 * 1024;

fn the_codec_named_on_the_command_line() -> Result<PosterFrameVideoCodec, String> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.as_slice() {
        [argument, codec] if argument == VIDEO_POSTER_FRAME_HELPER_CODEC_ARGUMENT => {
            PosterFrameVideoCodec::of_wire_name(codec).ok_or_else(|| {
                format!("`{codec}` is no codec this helper decodes; it decodes h264 and h265")
            })
        }
        _ => Err(format!(
            "the helper takes `{VIDEO_POSTER_FRAME_HELPER_CODEC_ARGUMENT} h264|h265` and an access unit on standard input"
        )),
    }
}

#[cfg(target_os = "linux")]
fn decode_the_first_picture(
    codec: PosterFrameVideoCodec,
    annex_b_access_unit: &[u8],
) -> Result<RgbaPictureFromThePosterFrameHelper, String> {
    linux::vulkan_video_poster_frame_decode::decode_the_first_picture_on_the_first_vulkan_video_device(
        codec,
        annex_b_access_unit,
    )
}

#[cfg(target_os = "macos")]
fn decode_the_first_picture(
    codec: PosterFrameVideoCodec,
    annex_b_access_unit: &[u8],
) -> Result<RgbaPictureFromThePosterFrameHelper, String> {
    apple::videotoolbox_poster_frame_decode::decode_the_first_picture_on_videotoolbox(
        codec,
        annex_b_access_unit,
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn decode_the_first_picture(
    _codec: PosterFrameVideoCodec,
    _annex_b_access_unit: &[u8],
) -> Result<RgbaPictureFromThePosterFrameHelper, String> {
    Err("this platform has no hardware decoder the runtime drives".to_owned())
}

fn decode_the_access_unit_on_standard_input() -> Result<RgbaPictureFromThePosterFrameHelper, String>
{
    let codec = the_codec_named_on_the_command_line()?;
    let mut annex_b_access_unit = Vec::new();
    std::io::stdin()
        .lock()
        .take(LARGEST_ACCESS_UNIT_READ_BYTES)
        .read_to_end(&mut annex_b_access_unit)
        .map_err(|why| format!("the access unit did not read from standard input: {why}"))?;
    if annex_b_access_unit.is_empty() {
        return Err("standard input held no access unit".to_owned());
    }
    decode_the_first_picture(codec, &annex_b_access_unit)
}

fn main() -> ExitCode {
    let written = decode_the_access_unit_on_standard_input().and_then(|picture| {
        picture
            .write_to(&mut std::io::stdout().lock())
            .map_err(|why| format!("the picture did not write to standard output: {why}"))
    });
    match written {
        Ok(()) => ExitCode::SUCCESS,
        Err(refusal) => {
            let mut standard_error = std::io::stderr().lock();
            let _ = writeln!(standard_error, "{refusal}");
            let _ = standard_error.flush();
            ExitCode::FAILURE
        }
    }
}
