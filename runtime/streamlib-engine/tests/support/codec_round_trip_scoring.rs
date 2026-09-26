// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What every codec-arm round-trip test measures a decode with — the
//! checked-in PSNR references and the per-plane PSNR `cargo xtask psnr score`
//! takes — and the checked-in access-unit clips one floor's encoder wrote for
//! the other floor's decoder to read.

// Each test binary compiles its own copy and uses part of it.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use streamlib::sdk::context::{
    GpuContext, VideoCodecElementaryStream, VideoDecodeSessionRequest, probe_video_codec_backend,
};

/// The bands `xtask psnr score` fails a decode below.
pub const LUMA_PSNR_FAIL_FLOOR_DB: f64 = 30.0;
pub const CHROMA_PSNR_FAIL_FLOOR_DB: f64 = 30.0;

/// The extent of the checked-in cross-floor clips: 180 lines is a multiple of
/// neither codec's block, so a clip carries a conformance crop.
pub const CROSS_FLOOR_CLIP_WIDTH: u32 = 320;
pub const CROSS_FLOOR_CLIP_HEIGHT: u32 = 180;

/// The PSNR reference the cross-floor clips encode, cropped to their extent.
pub const CROSS_FLOOR_CLIP_REFERENCE: &str = "complex_pattern";

/// A picture as tightly packed RGBA8.
#[derive(Debug, Clone)]
pub struct Rgba8Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Rgba8Picture {
    /// A PNG's pixels, whatever its colour type.
    pub fn read_png(path: &Path) -> Self {
        let mut decoder = png::Decoder::new(std::fs::File::open(path).expect("the PNG opens"));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().expect("a PNG header");
        let mut buffer = vec![0u8; reader.output_buffer_size()];
        let frame = reader.next_frame(&mut buffer).expect("a PNG frame");
        let pixels = &buffer[..frame.buffer_size()];
        let rgba = match frame.color_type {
            png::ColorType::Rgba => pixels.to_vec(),
            png::ColorType::Rgb => pixels
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|&[red, green, blue]| [red, green, blue, 255])
                .collect(),
            png::ColorType::Grayscale => pixels
                .iter()
                .flat_map(|&gray| [gray, gray, gray, 255])
                .collect(),
            png::ColorType::GrayscaleAlpha => pixels
                .as_chunks::<2>()
                .0
                .iter()
                .flat_map(|&[gray, alpha]| [gray, gray, gray, alpha])
                .collect(),
            png::ColorType::Indexed => unreachable!("EXPAND resolves the palette"),
        };
        Self {
            width: frame.width,
            height: frame.height,
            rgba,
        }
    }

    /// The top-left `width` × `height` of this picture.
    pub fn cropped_to(&self, width: u32, height: u32) -> Self {
        assert!(width <= self.width && height <= self.height);
        let row_bytes = self.width as usize * 4;
        let rgba = (0..height as usize)
            .flat_map(|row| &self.rgba[row * row_bytes..row * row_bytes + width as usize * 4])
            .copied()
            .collect();
        Self {
            width,
            height,
            rgba,
        }
    }

    pub fn write_png(&self, path: &Path) {
        let file = std::fs::File::create(path).expect("the PNG is creatable");
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .and_then(|mut writer| writer.write_image_data(&self.rgba))
            .expect("the PNG is written");
    }
}

/// Every checked-in PSNR reference, sorted by name.
pub fn psnr_reference_paths() -> Vec<PathBuf> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/psnr");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&directory)
        .expect("the checked-in references")
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "png"))
        .collect();
    paths.sort();
    assert!(
        !paths.is_empty(),
        "no references in {}",
        directory.display()
    );
    paths
}

/// The reference a cross-floor clip encodes: [`CROSS_FLOOR_CLIP_REFERENCE`]
/// cropped to the clip's extent.
pub fn cross_floor_clip_reference_picture() -> Rgba8Picture {
    Rgba8Picture::read_png(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/psnr")
            .join(format!("{CROSS_FLOOR_CLIP_REFERENCE}.png")),
    )
    .cropped_to(CROSS_FLOOR_CLIP_WIDTH, CROSS_FLOOR_CLIP_HEIGHT)
}

/// Where the checked-in clip VideoToolbox encoded for `elementary_stream`
/// lives.
pub fn videotoolbox_cross_floor_clip_path(
    elementary_stream: VideoCodecElementaryStream,
) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/cross_floor_codec_clips")
        .join(match elementary_stream {
            VideoCodecElementaryStream::H264 => "videotoolbox_h264_320x180.access_units",
            VideoCodecElementaryStream::H265 => "videotoolbox_h265_320x180.access_units",
        })
}

/// Write access units as a clip: each one's byte count as a big-endian `u32`,
/// then its Annex-B bytes, so a reader hands a decoder one access unit at a
/// time exactly as the encoder published them.
pub fn write_access_unit_clip(path: &Path, access_units: &[Vec<u8>]) {
    let mut clip = Vec::new();
    for access_unit in access_units {
        clip.extend_from_slice(
            &u32::try_from(access_unit.len())
                .expect("an access unit fits a u32 length")
                .to_be_bytes(),
        );
        clip.extend_from_slice(access_unit);
    }
    std::fs::write(path, clip).expect("the clip is written");
}

/// The access units a clip [`write_access_unit_clip`] wrote holds, in order.
pub fn read_access_unit_clip(path: &Path) -> Vec<Vec<u8>> {
    let clip = std::fs::read(path).expect("the checked-in clip");
    let mut access_units = Vec::new();
    let mut remaining = clip.as_slice();
    while !remaining.is_empty() {
        let (length, after_length) = remaining.split_at(4);
        let length = u32::from_be_bytes(length.try_into().expect("four bytes")) as usize;
        let (access_unit, after_access_unit) = after_length.split_at(length);
        access_units.push(access_unit.to_vec());
        remaining = after_access_unit;
    }
    access_units
}

/// Per-plane PSNR between two RGBA8 pictures of one extent, over their
/// BT.709 full-range 4:2:0 planes.
#[derive(Debug)]
pub struct Yuv420PlanePsnr {
    pub luma_db: f64,
    pub blue_difference_db: f64,
    pub red_difference_db: f64,
}

impl Yuv420PlanePsnr {
    pub fn between(decoded_rgba: &[u8], reference: &Rgba8Picture) -> Self {
        let decoded =
            bt709_full_range_yuv420_planes(decoded_rgba, reference.width, reference.height);
        let reference_planes =
            bt709_full_range_yuv420_planes(&reference.rgba, reference.width, reference.height);
        Self {
            luma_db: peak_signal_to_noise_ratio_db(&decoded[0], &reference_planes[0]),
            blue_difference_db: peak_signal_to_noise_ratio_db(&decoded[1], &reference_planes[1]),
            red_difference_db: peak_signal_to_noise_ratio_db(&decoded[2], &reference_planes[2]),
        }
    }

    /// Whether any plane falls below the band `xtask psnr score` fails.
    pub fn fails_the_bands(&self) -> bool {
        self.luma_db < LUMA_PSNR_FAIL_FLOOR_DB
            || self.blue_difference_db < CHROMA_PSNR_FAIL_FLOOR_DB
            || self.red_difference_db < CHROMA_PSNR_FAIL_FLOOR_DB
    }
}

/// Y at full resolution, then Cb and Cr box-averaged to half, each rounded to
/// eight bits. `width` and `height` are even.
fn bt709_full_range_yuv420_planes(rgba: &[u8], width: u32, height: u32) -> [Vec<u8>; 3] {
    let (width, height) = (width as usize, height as usize);
    let (mut luma, mut blue_difference, mut red_difference) = (
        vec![0f64; width * height],
        vec![0f64; width * height],
        vec![0f64; width * height],
    );
    for (index, &[red, green, blue, _alpha]) in rgba
        .as_chunks::<4>()
        .0
        .iter()
        .take(width * height)
        .enumerate()
    {
        let [red, green, blue] = [red, green, blue].map(f64::from);
        let y = 0.2126 * red + 0.7152 * green + 0.0722 * blue;
        luma[index] = y;
        blue_difference[index] = (blue - y) / 1.8556 + 128.0;
        red_difference[index] = (red - y) / 1.5748 + 128.0;
    }
    let half_resolution = |plane: &[f64]| -> Vec<u8> {
        let mut halved = Vec::with_capacity(width * height / 4);
        for row in (0..height).step_by(2) {
            for column in (0..width).step_by(2) {
                let sum = plane[row * width + column]
                    + plane[row * width + column + 1]
                    + plane[(row + 1) * width + column]
                    + plane[(row + 1) * width + column + 1];
                halved.push((sum / 4.0).round().clamp(0.0, 255.0) as u8);
            }
        }
        halved
    };
    [
        luma.iter()
            .map(|y| y.round().clamp(0.0, 255.0) as u8)
            .collect(),
        half_resolution(&blue_difference),
        half_resolution(&red_difference),
    ]
}

fn peak_signal_to_noise_ratio_db(decoded: &[u8], reference: &[u8]) -> f64 {
    let mean_squared_error = decoded
        .iter()
        .zip(reference)
        .map(|(&decoded, &reference)| (f64::from(decoded) - f64::from(reference)).powi(2))
        .sum::<f64>()
        / decoded.len() as f64;
    if mean_squared_error == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0f64.powi(2) / mean_squared_error).log10()
}

/// Decode the clip VideoToolbox wrote for `elementary_stream` through
/// whichever arm this platform's chain probed, and hold every access unit to
/// one picture and the last picture to the bands.
pub fn decode_the_checked_in_videotoolbox_clip_inside_the_bands(
    gpu_context: &GpuContext,
    elementary_stream: VideoCodecElementaryStream,
) {
    let backend = probe_video_codec_backend();
    let gpu = gpu_context.limited_access();
    let access_units =
        read_access_unit_clip(&videotoolbox_cross_floor_clip_path(elementary_stream));
    let mut decode_session = gpu
        .escalate(|full| {
            backend.open_decode_session(
                full,
                &VideoDecodeSessionRequest {
                    elementary_stream,
                    maximum_coded_extent: None,
                },
            )
        })
        .expect("a decode session");
    let mut decoded_pictures = Vec::new();
    for access_unit in &access_units {
        decode_session
            .decode_annex_b_access_unit(access_unit, &mut decoded_pictures)
            .expect("the access unit decodes");
    }
    assert_eq!(
        decoded_pictures.len(),
        access_units.len(),
        "every access unit of the clip decodes to one picture on the {} arm",
        backend.backend_name()
    );
    let last_picture = decoded_pictures.last().expect("a decoded picture");
    assert_eq!(
        (last_picture.width, last_picture.height),
        (CROSS_FLOOR_CLIP_WIDTH, CROSS_FLOOR_CLIP_HEIGHT)
    );
    // SAFETY: the pooled picture is held, host-readable once decode hands it
    // back, and exactly `width × height × 4` bytes.
    let decoded_rgba = unsafe {
        std::slice::from_raw_parts(
            last_picture.pixel_buffer.plane_base_address(0),
            CROSS_FLOOR_CLIP_WIDTH as usize * CROSS_FLOOR_CLIP_HEIGHT as usize * 4,
        )
    };
    let scored = Yuv420PlanePsnr::between(decoded_rgba, &cross_floor_clip_reference_picture());
    assert!(
        !scored.fails_the_bands(),
        "{elementary_stream:?}: {scored:?}"
    );
}
