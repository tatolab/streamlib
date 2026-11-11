// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A video's poster frame: its first sync sample decoded by the runtime unit's
//! `tatolab-video-poster-frame` helper on the machine's hardware decoder, out of the runtime's
//! process, and scaled into a preview picture.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::first_video_sync_sample::{
    FirstVideoSyncSampleAsAnnexB, first_video_sync_sample_of_a_video_file,
};
use crate::image_file_media_facts::preview_picture_of;
use crate::{AssetMediaFactsUnreadable, AssetPreviewPictureMadeFromTheFile};

/// The file name of the helper the runtime unit carries beside `tatolabd`.
pub const VIDEO_POSTER_FRAME_HELPER_EXECUTABLE_FILE_NAME: &str = "tatolab-video-poster-frame";

/// The helper's one argument, followed by the codec's wire name (`h264`, `h265`).
pub const VIDEO_POSTER_FRAME_HELPER_CODEC_ARGUMENT: &str = "--codec";

/// How long the helper may take to decode one access unit before it is killed.
const VIDEO_POSTER_FRAME_HELPER_TIME_ALLOWED: Duration = Duration::from_secs(20);

/// The longest edge of a picture the helper answers, which bounds what the runtime reads back.
const LARGEST_POSTER_FRAME_EDGE_PIXELS: u32 = 16_384;

/// One RGBA picture the helper writes to its standard output: its width and height as
/// little-endian `u32`s, then its rows of 8-bit RGBA, top to bottom, packed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaPictureFromThePosterFrameHelper {
    /// Its width in pixels.
    pub pixel_width: u32,
    /// Its height in pixels.
    pub pixel_height: u32,
    /// Its pixels, `pixel_width * pixel_height * 4` bytes.
    pub rgba_bytes: Vec<u8>,
}

impl RgbaPictureFromThePosterFrameHelper {
    /// Write the picture as the helper hands it to the runtime.
    pub fn write_to(&self, writer: &mut impl Write) -> std::io::Result<()> {
        writer.write_all(&self.pixel_width.to_le_bytes())?;
        writer.write_all(&self.pixel_height.to_le_bytes())?;
        writer.write_all(&self.rgba_bytes)?;
        writer.flush()
    }

    /// The picture the helper wrote as `written`, or why it is none.
    pub fn read_from(written: &[u8]) -> Result<Self, String> {
        let (pixel_width, rest) = written
            .split_first_chunk::<4>()
            .ok_or("the helper wrote no picture")?;
        let (pixel_height, rgba_bytes) = rest
            .split_first_chunk::<4>()
            .ok_or("the helper wrote no picture height")?;
        let (pixel_width, pixel_height) = (
            u32::from_le_bytes(*pixel_width),
            u32::from_le_bytes(*pixel_height),
        );
        if pixel_width == 0
            || pixel_height == 0
            || pixel_width > LARGEST_POSTER_FRAME_EDGE_PIXELS
            || pixel_height > LARGEST_POSTER_FRAME_EDGE_PIXELS
        {
            return Err(format!(
                "the helper wrote a {pixel_width}×{pixel_height} picture, which no poster frame is"
            ));
        }
        let expected_len = pixel_width as usize * pixel_height as usize * 4;
        if rgba_bytes.len() != expected_len {
            return Err(format!(
                "the helper wrote {} bytes of a {pixel_width}×{pixel_height} RGBA picture, not {expected_len}",
                rgba_bytes.len()
            ));
        }
        Ok(Self {
            pixel_width,
            pixel_height,
            rgba_bytes: rgba_bytes.to_vec(),
        })
    }
}

/// The colour an NV12 picture's samples are coded in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Nv12PictureColour {
    /// The H.273 `matrix_coefficients` code point: 1 BT.709, 5 or 6 BT.601, 9 BT.2020; any other
    /// is read as BT.709.
    pub h273_matrix_coefficients: Option<u8>,
    /// Whether luma spans 0–255 rather than 16–235.
    pub full_range: bool,
}

/// An NV12 picture's planes as 8-bit RGBA, top to bottom, packed; `None` when a plane is
/// shorter than its stride and the picture's extent say.
pub fn rgba_of_an_nv12_picture(
    pixel_width: u32,
    pixel_height: u32,
    luma_plane: &[u8],
    luma_row_stride_bytes: usize,
    chroma_plane: &[u8],
    chroma_row_stride_bytes: usize,
    colour: Nv12PictureColour,
) -> Option<Vec<u8>> {
    let (red_from_cr, green_from_cb, green_from_cr, blue_from_cb) =
        match colour.h273_matrix_coefficients {
            Some(5 | 6) => (1.402, 0.344_136, 0.714_136, 1.772),
            Some(9) => (1.4746, 0.164_553, 0.571_353, 1.8814),
            _ => (1.5748, 0.187_324, 0.468_124, 1.8556),
        };
    let (luma_offset, luma_scale, chroma_scale) = if colour.full_range {
        (0.0, 1.0, 1.0)
    } else {
        (16.0, 255.0 / 219.0, 255.0 / 224.0)
    };
    let (width, height) = (pixel_width as usize, pixel_height as usize);
    let mut rgba = Vec::with_capacity(width * height * 4);
    for row in 0..height {
        let luma_row = luma_plane
            .get(row * luma_row_stride_bytes..)?
            .get(..width)?;
        let chroma_row = chroma_plane
            .get((row / 2) * chroma_row_stride_bytes..)?
            .get(..width.div_ceil(2) * 2)?;
        for (column, luma) in luma_row.iter().enumerate() {
            let cb = (f32::from(chroma_row[column / 2 * 2]) - 128.0) * chroma_scale;
            let cr = (f32::from(chroma_row[column / 2 * 2 + 1]) - 128.0) * chroma_scale;
            let y = (f32::from(*luma) - luma_offset) * luma_scale;
            let to_byte = |value: f32| value.round().clamp(0.0, 255.0) as u8;
            rgba.extend_from_slice(&[
                to_byte(y + red_from_cr * cr),
                to_byte(y - green_from_cb * cb - green_from_cr * cr),
                to_byte(y + blue_from_cb * cb),
                255,
            ]);
        }
    }
    Some(rgba)
}

/// The runtime unit's poster frame helper, run once per video read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoPosterFrameHelper {
    executable: PathBuf,
}

impl VideoPosterFrameHelper {
    /// The helper at `executable`.
    pub fn at(executable: PathBuf) -> Self {
        Self { executable }
    }

    /// The poster frame of the video file at `video_file_path`, as a preview picture.
    pub fn poster_frame_of_a_video_file(
        &self,
        video_file_path: &Path,
    ) -> Result<AssetPreviewPictureMadeFromTheFile, AssetMediaFactsUnreadable> {
        let first_sync_sample = first_video_sync_sample_of_a_video_file(video_file_path)?;
        let decoded = self
            .decode(&first_sync_sample)
            .map_err(AssetMediaFactsUnreadable::because)?;
        let picture = image::RgbaImage::from_raw(
            decoded.pixel_width,
            decoded.pixel_height,
            decoded.rgba_bytes,
        )
        .ok_or_else(|| AssetMediaFactsUnreadable::because("the decoded picture did not fit"))?;
        preview_picture_of(&image::DynamicImage::ImageRgba8(picture)).ok_or_else(|| {
            AssetMediaFactsUnreadable::because("the poster frame did not encode as PNG")
        })
    }

    fn decode(
        &self,
        first_sync_sample: &FirstVideoSyncSampleAsAnnexB,
    ) -> Result<RgbaPictureFromThePosterFrameHelper, String> {
        let mut helper = Command::new(&self.executable)
            .arg(VIDEO_POSTER_FRAME_HELPER_CODEC_ARGUMENT)
            .arg(first_sync_sample.codec.wire_name())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|why| {
                format!(
                    "the poster frame helper {} did not start: {why}",
                    self.executable.display()
                )
            })?;
        let (Some(mut helper_input), Some(mut helper_output), Some(mut helper_refusal)) = (
            helper.stdin.take(),
            helper.stdout.take(),
            helper.stderr.take(),
        ) else {
            let _ = helper.kill();
            let _ = helper.wait();
            return Err("the poster frame helper's pipes did not open".to_owned());
        };
        let access_unit = first_sync_sample.annex_b_access_unit.clone();
        let input_written = std::thread::spawn(move || helper_input.write_all(&access_unit));
        let refusal_read = std::thread::spawn(move || {
            let mut refusal = String::new();
            let _ = helper_refusal.read_to_string(&mut refusal);
            refusal
        });
        let (output_read, output_read_by_the_reader) = mpsc::channel();
        std::thread::spawn(move || {
            let mut output = Vec::new();
            let read = helper_output.read_to_end(&mut output).map(|_| output);
            let _ = output_read.send(read);
        });
        let output =
            match output_read_by_the_reader.recv_timeout(VIDEO_POSTER_FRAME_HELPER_TIME_ALLOWED) {
                Ok(read) => read
                    .map_err(|why| format!("the poster frame helper's output did not read: {why}")),
                Err(_) => {
                    let _ = helper.kill();
                    Err(format!(
                        "the poster frame helper did not finish within {} s and was stopped",
                        VIDEO_POSTER_FRAME_HELPER_TIME_ALLOWED.as_secs()
                    ))
                }
            };
        let exit = helper.wait();
        let _ = input_written.join();
        let refusal = refusal_read.join().unwrap_or_default();
        let output = output?;
        match exit {
            Ok(status) if status.success() => {
                RgbaPictureFromThePosterFrameHelper::read_from(&output)
            }
            Ok(status) => Err(format!(
                "the poster frame helper refused ({status}): {}",
                refusal.trim()
            )),
            Err(why) => Err(format!("the poster frame helper was lost: {why}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_round_trips_through_the_helper_wire() {
        let picture = RgbaPictureFromThePosterFrameHelper {
            pixel_width: 2,
            pixel_height: 1,
            rgba_bytes: vec![1, 2, 3, 255, 4, 5, 6, 255],
        };
        let mut written = Vec::new();
        picture.write_to(&mut written).unwrap();
        assert_eq!(
            RgbaPictureFromThePosterFrameHelper::read_from(&written),
            Ok(picture)
        );
        assert!(RgbaPictureFromThePosterFrameHelper::read_from(&written[..9]).is_err());
    }

    #[test]
    fn limited_range_bt709_white_black_and_red_convert_to_rgba() {
        let luma_plane = [235, 16, 63, 63];
        let chroma_plane = [128, 128, 102, 240];
        let colour = Nv12PictureColour {
            h273_matrix_coefficients: Some(1),
            full_range: false,
        };
        let top_row =
            rgba_of_an_nv12_picture(2, 1, &luma_plane[..2], 2, &chroma_plane[..2], 2, colour)
                .unwrap();
        assert_eq!(top_row, [255, 255, 255, 255, 0, 0, 0, 255]);
        let red = rgba_of_an_nv12_picture(2, 1, &luma_plane[2..], 2, &chroma_plane[2..], 2, colour)
            .unwrap();
        assert!(red[0] > 250 && red[1] < 5 && red[2] < 5, "{red:?}");
        assert_eq!(
            rgba_of_an_nv12_picture(4, 1, &luma_plane, 4, &chroma_plane[..2], 4, colour),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_helper_that_refuses_is_answered_in_its_own_words() {
        let directory = tempfile::tempdir().unwrap();
        let refusing_helper = directory.path().join("refusing-helper");
        std::fs::write(
            &refusing_helper,
            "#!/bin/sh\ncat >/dev/null\necho 'no hardware decoder' >&2\nexit 3\n",
        )
        .unwrap();
        std::fs::set_permissions(
            &refusing_helper,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let refusal = VideoPosterFrameHelper::at(refusing_helper)
            .decode(&FirstVideoSyncSampleAsAnnexB {
                codec: crate::PosterFrameVideoCodec::H264,
                annex_b_access_unit: vec![0, 0, 0, 1, 0x65],
            })
            .unwrap_err();
        assert!(refusal.ends_with("no hardware decoder"), "{refusal}");
    }

    #[cfg(unix)]
    #[test]
    fn a_picture_the_helper_writes_is_read_back() {
        let directory = tempfile::tempdir().unwrap();
        let decoding_helper = directory.path().join("decoding-helper");
        std::fs::write(
            &decoding_helper,
            "#!/bin/sh\ncat >/dev/null\nprintf '\\001\\000\\000\\000\\001\\000\\000\\000\\377\\000\\000\\377'\n",
        )
        .unwrap();
        std::fs::set_permissions(
            &decoding_helper,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        assert_eq!(
            VideoPosterFrameHelper::at(decoding_helper).decode(&FirstVideoSyncSampleAsAnnexB {
                codec: crate::PosterFrameVideoCodec::H265,
                annex_b_access_unit: vec![0, 0, 0, 1, 0x26, 0x01],
            }),
            Ok(RgbaPictureFromThePosterFrameHelper {
                pixel_width: 1,
                pixel_height: 1,
                rgba_bytes: vec![255, 0, 0, 255],
            })
        );
    }
}
