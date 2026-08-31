// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The image measurements the codec proof scores with: per-plane PSNR against
//! a reference frame, and the channel-mean drift lock the vivid colorimetry
//! rig compares to its baseline.
//!
//! Both live here rather than in two tools because the bug-injection modes are
//! shared, and a mode defined twice is a mode that can disagree with itself.
//! They took ffmpeg and ImageMagick out of the scoring path: a rig failure is
//! now never ambiguous between "the codec regressed" and "the scorer's
//! external tool changed its defaults under us".
//!
//! Every conversion is BT.709 full-range, applied identically to both sides of
//! a comparison. That is the colour the rig's fixture source declares on every
//! frame it publishes (`ColorInfo { Bt709, Srgb, Full }`), so a score measures
//! the codec round trip rather than a matrix disagreement between the scorer
//! and the pipeline.

use std::path::Path;

use anyhow::{Context, Result};

/// Y-plane PSNR at or above which a round trip is a clean pass, in dB.
pub const LUMA_PSNR_PASS_FLOOR_DB: f64 = 35.0;

/// Y-plane PSNR below which a round trip is a regression, in dB. Between this
/// and [`LUMA_PSNR_PASS_FLOOR_DB`] the result is a warning: visible enough to
/// investigate, not bad enough to have obviously broken.
pub const LUMA_PSNR_WARN_FLOOR_DB: f64 = 30.0;

/// The largest value an 8-bit sample can take, which is the peak the ratio is
/// taken against.
const EIGHT_BIT_SAMPLE_PEAK: f64 = 255.0;

/// One frame as tightly-packed 8-bit RGBA, the form every measurement here
/// starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba8Image {
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub rgba_bytes: Vec<u8>,
}

impl Rgba8Image {
    /// Build an image from bytes, refusing a buffer that does not hold exactly
    /// `pixel_width × pixel_height` RGBA pixels.
    pub fn from_rgba_bytes(
        pixel_width: u32,
        pixel_height: u32,
        rgba_bytes: Vec<u8>,
    ) -> Result<Self> {
        let expected_byte_len = (pixel_width as usize) * (pixel_height as usize) * 4;
        anyhow::ensure!(
            rgba_bytes.len() == expected_byte_len,
            "a {pixel_width}x{pixel_height} RGBA image is {expected_byte_len} bytes, got {}",
            rgba_bytes.len()
        );
        Ok(Self {
            pixel_width,
            pixel_height,
            rgba_bytes,
        })
    }

    /// Read a PNG as RGBA8.
    ///
    /// The reference set is mixed — palette, 1-bit and 16-bit greyscale,
    /// truecolour, because ImageMagick picks the narrowest encoding that fits
    /// each fixture — so every colour type normalises to 8 bits and then
    /// widens to RGBA. A scorer that read only truecolour PNGs would score the
    /// solid fixtures as unreadable rather than as passing.
    pub fn read_png(png_path: &Path) -> Result<Self> {
        let file = std::fs::File::open(png_path)
            .with_context(|| format!("opening {}", png_path.display()))?;
        let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = decoder
            .read_info()
            .with_context(|| format!("{} is not a readable PNG", png_path.display()))?;

        let mut decoded = vec![0u8; reader.output_buffer_size()];
        let decoded_frame = reader
            .next_frame(&mut decoded)
            .with_context(|| format!("{} did not decode", png_path.display()))?;
        decoded.truncate(decoded_frame.buffer_size());

        let (pixel_width, pixel_height) = (decoded_frame.width, decoded_frame.height);
        let pixel_count = (pixel_width as usize) * (pixel_height as usize);
        let rgba_bytes = match decoded_frame.color_type {
            png::ColorType::Rgba => decoded,
            png::ColorType::Rgb => widen_to_rgba8(&decoded, pixel_count, |[red, green, blue]| {
                [red, green, blue, 0xFF]
            }),
            png::ColorType::Grayscale => {
                widen_to_rgba8(&decoded, pixel_count, |[luma]| [luma, luma, luma, 0xFF])
            }
            png::ColorType::GrayscaleAlpha => {
                widen_to_rgba8(&decoded, pixel_count, |[luma, alpha]| {
                    [luma, luma, luma, alpha]
                })
            }
            unexpected => anyhow::bail!(
                "{} normalised to {unexpected:?}, which this reader does not widen to RGBA",
                png_path.display()
            ),
        };
        Self::from_rgba_bytes(pixel_width, pixel_height, rgba_bytes)
    }

    /// The top-left `pixel_width × pixel_height` corner.
    ///
    /// This is the conformance crop a scorer owes an H.265 decode: a CTU-padded
    /// stream codes 1920x1088 for a 1920x1080 picture, and the padding rows
    /// carry content nobody encoded. Scoring them would measure the padding.
    pub fn cropped_to(&self, pixel_width: u32, pixel_height: u32) -> Result<Self> {
        anyhow::ensure!(
            pixel_width <= self.pixel_width && pixel_height <= self.pixel_height,
            "cannot crop a {}x{} image up to {pixel_width}x{pixel_height}",
            self.pixel_width,
            self.pixel_height
        );
        if (pixel_width, pixel_height) == (self.pixel_width, self.pixel_height) {
            return Ok(self.clone());
        }
        let source_row_byte_len = (self.pixel_width as usize) * 4;
        let cropped_row_byte_len = (pixel_width as usize) * 4;
        let mut rgba_bytes = Vec::with_capacity(cropped_row_byte_len * (pixel_height as usize));
        for row in 0..(pixel_height as usize) {
            let row_start = row * source_row_byte_len;
            rgba_bytes
                .extend_from_slice(&self.rgba_bytes[row_start..row_start + cropped_row_byte_len]);
        }
        Self::from_rgba_bytes(pixel_width, pixel_height, rgba_bytes)
    }

    /// Mean of each RGB channel over every pixel, on the `[0, 1]` scale the
    /// vivid baseline TSV is written in.
    pub fn rgb_channel_means(&self) -> RgbChannelMeans {
        let mut totals = [0u64; 3];
        for pixel in self.rgba_bytes.chunks_exact(4) {
            totals[0] += u64::from(pixel[0]);
            totals[1] += u64::from(pixel[1]);
            totals[2] += u64::from(pixel[2]);
        }
        let pixel_count = (self.rgba_bytes.len() / 4).max(1) as f64;
        RgbChannelMeans {
            red: totals[0] as f64 / pixel_count / EIGHT_BIT_SAMPLE_PEAK,
            green: totals[1] as f64 / pixel_count / EIGHT_BIT_SAMPLE_PEAK,
            blue: totals[2] as f64 / pixel_count / EIGHT_BIT_SAMPLE_PEAK,
        }
    }

    /// Convert to 4:2:0 Y/U/V planes for scoring.
    ///
    /// 4:2:0 rather than the full-resolution chroma this starts from because
    /// that is the sampling the codecs actually carry, so the U/V columns of a
    /// report describe the planes that crossed the wire.
    pub fn to_bt709_full_range_yuv420_planes(&self) -> Yuv420Planes {
        let pixel_count = (self.pixel_width as usize) * (self.pixel_height as usize);
        let mut luma_plane = vec![0u8; pixel_count];
        let mut full_resolution_blue_difference = vec![0f32; pixel_count];
        let mut full_resolution_red_difference = vec![0f32; pixel_count];

        for (index, pixel) in self.rgba_bytes.chunks_exact(4).enumerate() {
            let [luma, blue_difference, red_difference] = BT709_LUMA_COEFFICIENTS
                .rgb_to_yuv_full_range([
                    f32::from(pixel[0]),
                    f32::from(pixel[1]),
                    f32::from(pixel[2]),
                ]);
            luma_plane[index] = round_to_u8(luma);
            full_resolution_blue_difference[index] = blue_difference;
            full_resolution_red_difference[index] = red_difference;
        }

        Yuv420Planes {
            luma_plane,
            blue_difference_chroma_plane: box_average_to_half_resolution(
                &full_resolution_blue_difference,
                self.pixel_width,
                self.pixel_height,
            ),
            red_difference_chroma_plane: box_average_to_half_resolution(
                &full_resolution_red_difference,
                self.pixel_width,
                self.pixel_height,
            ),
        }
    }

    /// A copy carrying one deliberate colour-management regression.
    ///
    /// The rig injects these to prove the gate is live: each mode must drop the
    /// Y PSNR of at least one reference below the fail floor, or the gate is
    /// passing because it measures nothing.
    pub fn with_injected_color_regression(&self, regression: InjectedColorRegression) -> Self {
        let mut injected = self.clone();
        for pixel in injected.rgba_bytes.chunks_exact_mut(4) {
            let [red, green, blue] = match regression {
                InjectedColorRegression::SwapRedAndBlueChannels => {
                    [pixel[2], pixel[1], pixel[0]].map(f32::from)
                }
                InjectedColorRegression::Bt601EncodedDecodedAsBt709 => {
                    let yuv = BT601_LUMA_COEFFICIENTS.rgb_to_yuv_full_range([
                        f32::from(pixel[0]),
                        f32::from(pixel[1]),
                        f32::from(pixel[2]),
                    ]);
                    BT709_LUMA_COEFFICIENTS.yuv_to_rgb_full_range(quantized(yuv))
                }
                InjectedColorRegression::FullRangeEncodedDecodedAsLimitedRange => {
                    let [luma, blue_difference, red_difference] =
                        quantized(BT709_LUMA_COEFFICIENTS.rgb_to_yuv_full_range([
                            f32::from(pixel[0]),
                            f32::from(pixel[1]),
                            f32::from(pixel[2]),
                        ]));
                    BT709_LUMA_COEFFICIENTS.yuv_to_rgb_full_range([
                        expand_limited_range_luma(luma),
                        expand_limited_range_chroma(blue_difference),
                        expand_limited_range_chroma(red_difference),
                    ])
                }
            };
            pixel[0] = round_to_u8(red);
            pixel[1] = round_to_u8(green);
            pixel[2] = round_to_u8(blue);
        }
        injected
    }
}

/// Mean of each RGB channel over a frame, on the `[0, 1]` scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RgbChannelMeans {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
}

impl RgbChannelMeans {
    /// The three channels paired with the single-letter names the baseline TSV
    /// keys its rows by, in the order it writes them.
    pub fn by_baseline_channel_name(&self) -> [(&'static str, f64); 3] {
        [("r", self.red), ("g", self.green), ("b", self.blue)]
    }

    /// The arithmetic mean of a set of per-frame means — the rig-wide figure
    /// the drift lock compares, so one outlier frame cannot decide a run.
    pub fn averaged(per_frame_means: &[RgbChannelMeans]) -> Option<RgbChannelMeans> {
        if per_frame_means.is_empty() {
            return None;
        }
        let frame_count = per_frame_means.len() as f64;
        Some(RgbChannelMeans {
            red: per_frame_means.iter().map(|means| means.red).sum::<f64>() / frame_count,
            green: per_frame_means.iter().map(|means| means.green).sum::<f64>() / frame_count,
            blue: per_frame_means.iter().map(|means| means.blue).sum::<f64>() / frame_count,
        })
    }
}

/// 8-bit 4:2:0 planes, the sampling the codecs carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Yuv420Planes {
    pub luma_plane: Vec<u8>,
    pub blue_difference_chroma_plane: Vec<u8>,
    pub red_difference_chroma_plane: Vec<u8>,
}

/// A plane's peak signal-to-noise ratio against its reference.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlanePeakSignalToNoiseRatio {
    /// The two planes are byte-identical, so there is no noise and the ratio
    /// is infinite. Kept distinct from a large finite value because a lossless
    /// path and a very good lossy one are different claims.
    Identical,
    /// A finite ratio, in decibels.
    Decibels(f64),
}

impl PlanePeakSignalToNoiseRatio {
    /// The ratio of two equally-sized planes.
    pub fn between(measured_plane: &[u8], reference_plane: &[u8]) -> Result<Self> {
        anyhow::ensure!(
            measured_plane.len() == reference_plane.len(),
            "a {}-sample plane cannot be scored against a {}-sample reference",
            measured_plane.len(),
            reference_plane.len()
        );
        anyhow::ensure!(!measured_plane.is_empty(), "an empty plane has no ratio");

        let squared_error_total: u64 = measured_plane
            .iter()
            .zip(reference_plane)
            .map(|(measured, reference)| {
                let difference = i32::from(*measured) - i32::from(*reference);
                (difference * difference) as u64
            })
            .sum();
        if squared_error_total == 0 {
            return Ok(Self::Identical);
        }
        let mean_squared_error = squared_error_total as f64 / measured_plane.len() as f64;
        Ok(Self::Decibels(
            10.0 * (EIGHT_BIT_SAMPLE_PEAK * EIGHT_BIT_SAMPLE_PEAK / mean_squared_error).log10(),
        ))
    }

    /// Whether this ratio reaches a decibel floor. Identical planes reach every
    /// floor.
    pub fn reaches_floor_db(&self, floor_db: f64) -> bool {
        match self {
            Self::Identical => true,
            Self::Decibels(decibels) => *decibels >= floor_db,
        }
    }

    /// How a report column spells it.
    pub fn as_report_column(&self) -> String {
        match self {
            Self::Identical => "inf".to_string(),
            Self::Decibels(decibels) => format!("{decibels:.2}"),
        }
    }
}

/// How a decoded frame compares to the reference that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceComparisonVerdict {
    /// Y at or above [`LUMA_PSNR_PASS_FLOOR_DB`].
    Pass,
    /// Y between the warn and pass floors — investigate, do not gate on it.
    Warn,
    /// Y below [`LUMA_PSNR_WARN_FLOOR_DB`]: a colour-matrix, range, or
    /// plane-layout regression.
    Fail,
}

impl ReferenceComparisonVerdict {
    /// Classify a luma ratio against the decided floors.
    pub fn for_luma_ratio(luma_ratio: PlanePeakSignalToNoiseRatio) -> Self {
        if luma_ratio.reaches_floor_db(LUMA_PSNR_PASS_FLOOR_DB) {
            Self::Pass
        } else if luma_ratio.reaches_floor_db(LUMA_PSNR_WARN_FLOOR_DB) {
            Self::Warn
        } else {
            Self::Fail
        }
    }

    /// How a report column spells it.
    pub fn as_report_column(&self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
        }
    }
}

/// A deliberate colour-management regression the rig injects to prove its gate
/// is not vacuous. Each is a real mis-interpretation class the codec path has
/// shipped before, not a synthetic corruption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum InjectedColorRegression {
    /// R and B exchanged — the plane-order and texture-format-binding class.
    /// Leaves the greyscale references untouched, which is what makes it a
    /// chroma test rather than a smoke test.
    #[value(name = "swap-channels")]
    SwapRedAndBlueChannels,
    /// Encoded through the BT.601 matrix and decoded through BT.709 — the
    /// green/magenta tint class. Also greyscale-invariant.
    #[value(name = "bt601-bt709")]
    Bt601EncodedDecodedAsBt709,
    /// Encoded at full range and decoded as if limited, so the decoder expands
    /// 16-235 to fill 0-255 and clips both ends. Invisible on saturated
    /// colours and on black and white, which is why the gradients carry this
    /// one.
    #[value(name = "range-swap")]
    FullRangeEncodedDecodedAsLimitedRange,
}

impl InjectedColorRegression {
    /// Every mode, in the order the rig documents them.
    pub const ALL: [InjectedColorRegression; 3] = [
        InjectedColorRegression::SwapRedAndBlueChannels,
        InjectedColorRegression::Bt601EncodedDecodedAsBt709,
        InjectedColorRegression::FullRangeEncodedDecodedAsLimitedRange,
    ];

    /// The spelling the command line and the fixture scripts use.
    pub fn as_command_line_value(self) -> &'static str {
        match self {
            Self::SwapRedAndBlueChannels => "swap-channels",
            Self::Bt601EncodedDecodedAsBt709 => "bt601-bt709",
            Self::FullRangeEncodedDecodedAsLimitedRange => "range-swap",
        }
    }
}

/// Luma weights of one colour matrix, which is the whole difference between
/// BT.601 and BT.709 for these conversions.
struct LumaCoefficients {
    red: f32,
    green: f32,
    blue: f32,
}

const BT709_LUMA_COEFFICIENTS: LumaCoefficients = LumaCoefficients {
    red: 0.2126,
    green: 0.7152,
    blue: 0.0722,
};

const BT601_LUMA_COEFFICIENTS: LumaCoefficients = LumaCoefficients {
    red: 0.299,
    green: 0.587,
    blue: 0.114,
};

/// Where an 8-bit chroma sample sits when the difference is zero.
const CHROMA_ZERO_LEVEL: f32 = 128.0;

impl LumaCoefficients {
    /// RGB in `[0, 255]` to full-range Y/Cb/Cr, chroma centred on 128. Not
    /// rounded or clamped: an injection chains two of these, and quantising in
    /// between would attribute the codec's rounding to the injected bug.
    fn rgb_to_yuv_full_range(&self, [red, green, blue]: [f32; 3]) -> [f32; 3] {
        let luma = self.red * red + self.green * green + self.blue * blue;
        [
            luma,
            (blue - luma) / (2.0 * (1.0 - self.blue)) + CHROMA_ZERO_LEVEL,
            (red - luma) / (2.0 * (1.0 - self.red)) + CHROMA_ZERO_LEVEL,
        ]
    }

    /// The inverse of [`Self::rgb_to_yuv_full_range`], unclamped for the same
    /// reason.
    fn yuv_to_rgb_full_range(&self, [luma, blue_difference, red_difference]: [f32; 3]) -> [f32; 3] {
        let blue_difference = blue_difference - CHROMA_ZERO_LEVEL;
        let red_difference = red_difference - CHROMA_ZERO_LEVEL;
        [
            luma + 2.0 * (1.0 - self.red) * red_difference,
            luma - (2.0 * self.red * (1.0 - self.red) / self.green) * red_difference
                - (2.0 * self.blue * (1.0 - self.blue) / self.green) * blue_difference,
            luma + 2.0 * (1.0 - self.blue) * blue_difference,
        ]
    }
}

/// Round to the nearest 8-bit sample, saturating at both ends.
fn round_to_u8(sample: f32) -> u8 {
    sample.round().clamp(0.0, EIGHT_BIT_SAMPLE_PEAK as f32) as u8
}

/// Send a YUV triple through the 8-bit quantisation a real bitstream imposes.
///
/// The injected regressions are round trips through a wire, so the intermediate
/// has to be 8-bit: `solid_red` clips its Cr at 255 there, and a float-only
/// chain would score a bug the pipeline could not actually produce.
fn quantized(yuv: [f32; 3]) -> [f32; 3] {
    yuv.map(|sample| f32::from(round_to_u8(sample)))
}

/// Reinterpret a full-range luma sample as if it had been coded 16-235.
fn expand_limited_range_luma(luma: f32) -> f32 {
    (luma - 16.0) * (255.0 / 219.0)
}

/// Reinterpret a full-range chroma sample as if it had been coded 16-240.
fn expand_limited_range_chroma(chroma: f32) -> f32 {
    (chroma - CHROMA_ZERO_LEVEL) * (255.0 / 224.0) + CHROMA_ZERO_LEVEL
}

/// Average a full-resolution chroma plane down to 4:2:0 by 2x2 box, with the
/// last row and column of an odd extent averaging over the samples that exist.
fn box_average_to_half_resolution(
    full_resolution: &[f32],
    pixel_width: u32,
    pixel_height: u32,
) -> Vec<u8> {
    let (pixel_width, pixel_height) = (pixel_width as usize, pixel_height as usize);
    let chroma_width = pixel_width.div_ceil(2);
    let chroma_height = pixel_height.div_ceil(2);
    let mut chroma_plane = vec![0u8; chroma_width * chroma_height];

    for chroma_row in 0..chroma_height {
        for chroma_column in 0..chroma_width {
            let mut total = 0.0f32;
            let mut sample_count = 0.0f32;
            for row_offset in 0..2 {
                for column_offset in 0..2 {
                    let row = chroma_row * 2 + row_offset;
                    let column = chroma_column * 2 + column_offset;
                    if row < pixel_height && column < pixel_width {
                        total += full_resolution[row * pixel_width + column];
                        sample_count += 1.0;
                    }
                }
            }
            chroma_plane[chroma_row * chroma_width + chroma_column] =
                round_to_u8(total / sample_count);
        }
    }
    chroma_plane
}

/// Widen `SOURCE_BYTES_PER_PIXEL`-wide samples to RGBA8, one pixel at a time.
/// The source width is a const generic so a widening closure that reads more
/// channels than the caller declared is a compile error rather than an
/// out-of-bounds index.
fn widen_to_rgba8<const SOURCE_BYTES_PER_PIXEL: usize>(
    decoded: &[u8],
    pixel_count: usize,
    widen_one_pixel: impl Fn([u8; SOURCE_BYTES_PER_PIXEL]) -> [u8; 4],
) -> Vec<u8> {
    decoded
        .chunks_exact(SOURCE_BYTES_PER_PIXEL)
        .take(pixel_count)
        .flat_map(|source| {
            let source: [u8; SOURCE_BYTES_PER_PIXEL] =
                source.try_into().expect("chunks_exact yields exact widths");
            widen_one_pixel(source)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::ValueEnum;

    /// The checked-in reference set the codec proof scores against, resolved
    /// against this crate rather than whatever directory a test runner starts
    /// in.
    const CHECKED_IN_REFERENCE_DIRECTORY: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../runtime/streamlib-engine/tests/fixtures/psnr"
    );

    fn solid_color_image(pixel_width: u32, pixel_height: u32, rgb: [u8; 3]) -> Rgba8Image {
        let rgba_bytes = std::iter::repeat_n(
            [rgb[0], rgb[1], rgb[2], 0xFF],
            (pixel_width as usize) * (pixel_height as usize),
        )
        .flatten()
        .collect();
        Rgba8Image::from_rgba_bytes(pixel_width, pixel_height, rgba_bytes).unwrap()
    }

    /// A black-to-white ramp across the width, which is what
    /// `gradient_horizontal.png` is and the only content class the range swap
    /// shows up on.
    fn horizontal_luma_ramp_image(pixel_width: u32, pixel_height: u32) -> Rgba8Image {
        let mut rgba_bytes = Vec::with_capacity((pixel_width * pixel_height * 4) as usize);
        for _ in 0..pixel_height {
            for column in 0..pixel_width {
                let luma = ((column * 255) / (pixel_width - 1).max(1)) as u8;
                rgba_bytes.extend_from_slice(&[luma, luma, luma, 0xFF]);
            }
        }
        Rgba8Image::from_rgba_bytes(pixel_width, pixel_height, rgba_bytes).unwrap()
    }

    fn luma_ratio_against_itself_after_injecting(
        image: &Rgba8Image,
        regression: InjectedColorRegression,
    ) -> PlanePeakSignalToNoiseRatio {
        let reference_planes = image.to_bt709_full_range_yuv420_planes();
        let injected_planes = image
            .with_injected_color_regression(regression)
            .to_bt709_full_range_yuv420_planes();
        PlanePeakSignalToNoiseRatio::between(
            &injected_planes.luma_plane,
            &reference_planes.luma_plane,
        )
        .unwrap()
    }

    #[test]
    fn byte_identical_planes_score_as_infinite_rather_than_as_a_large_number() {
        let plane = [0u8, 17, 200, 255];
        assert_eq!(
            PlanePeakSignalToNoiseRatio::between(&plane, &plane).unwrap(),
            PlanePeakSignalToNoiseRatio::Identical
        );
        assert_eq!(
            PlanePeakSignalToNoiseRatio::Identical.as_report_column(),
            "inf"
        );
    }

    #[test]
    fn a_plane_off_by_one_everywhere_scores_the_ratio_the_definition_gives() {
        // MSE of 1 puts the ratio at 10*log10(255^2), which is the number every
        // other PSNR tool prints for this input.
        let measured = [10u8, 20, 30, 40];
        let reference = [11u8, 21, 31, 41];
        let PlanePeakSignalToNoiseRatio::Decibels(decibels) =
            PlanePeakSignalToNoiseRatio::between(&measured, &reference).unwrap()
        else {
            panic!("planes that differ are not identical");
        };
        assert!(
            (decibels - 48.130_803_608_679_02).abs() < 1e-9,
            "expected the textbook 48.13 dB, got {decibels}"
        );
    }

    #[test]
    fn the_verdict_reads_the_decided_floors_at_their_boundaries() {
        use PlanePeakSignalToNoiseRatio::Decibels;
        use ReferenceComparisonVerdict::{Fail, Pass, Warn};
        for (ratio, expected) in [
            (PlanePeakSignalToNoiseRatio::Identical, Pass),
            (Decibels(LUMA_PSNR_PASS_FLOOR_DB), Pass),
            (Decibels(LUMA_PSNR_PASS_FLOOR_DB - 0.01), Warn),
            (Decibels(LUMA_PSNR_WARN_FLOOR_DB), Warn),
            (Decibels(LUMA_PSNR_WARN_FLOOR_DB - 0.01), Fail),
        ] {
            assert_eq!(
                ReferenceComparisonVerdict::for_luma_ratio(ratio),
                expected,
                "{ratio:?} classified wrong"
            );
        }
    }

    #[test]
    fn planes_of_different_lengths_are_refused_rather_than_zipped_short() {
        let refusal = PlanePeakSignalToNoiseRatio::between(&[1u8, 2, 3], &[1u8, 2])
            .expect_err("a short reference must be refused, not silently truncated");
        assert!(
            refusal.to_string().contains("cannot be scored against"),
            "{refusal}"
        );
        PlanePeakSignalToNoiseRatio::between(&[], &[]).expect_err("an empty plane has no ratio");
    }

    #[test]
    fn a_ctu_padded_decode_crops_to_the_reference_extent_before_it_is_scored() {
        let reference = solid_color_image(8, 4, [30, 60, 90]);
        let mut padded = reference.rgba_bytes.clone();
        // Two rows of the garbage a CTU-padded stream carries past the crop.
        padded.extend(std::iter::repeat_n(0xA5u8, 8 * 2 * 4));
        let decoded = Rgba8Image::from_rgba_bytes(8, 6, padded).unwrap();

        assert_eq!(decoded.cropped_to(8, 4).unwrap(), reference);
        assert!(
            decoded.cropped_to(8, 8).is_err(),
            "cropping up would invent rows nobody decoded"
        );
    }

    #[test]
    fn swapping_red_and_blue_moves_a_saturated_frame_below_the_fail_floor() {
        let ratio = luma_ratio_against_itself_after_injecting(
            &solid_color_image(16, 16, [255, 0, 0]),
            InjectedColorRegression::SwapRedAndBlueChannels,
        );
        assert_eq!(
            ReferenceComparisonVerdict::for_luma_ratio(ratio),
            ReferenceComparisonVerdict::Fail,
            "R<->B on a saturated red must trip the gate; got {ratio:?}"
        );
    }

    #[test]
    fn the_bt601_bt709_mismatch_moves_a_saturated_frame_below_the_fail_floor() {
        let ratio = luma_ratio_against_itself_after_injecting(
            &solid_color_image(16, 16, [255, 0, 0]),
            InjectedColorRegression::Bt601EncodedDecodedAsBt709,
        );
        assert_eq!(
            ReferenceComparisonVerdict::for_luma_ratio(ratio),
            ReferenceComparisonVerdict::Fail,
            "a matrix mis-interpretation on a saturated red must trip the gate; got {ratio:?}"
        );
    }

    #[test]
    fn the_range_swap_moves_a_luma_ramp_below_the_fail_floor() {
        let ratio = luma_ratio_against_itself_after_injecting(
            &horizontal_luma_ramp_image(256, 8),
            InjectedColorRegression::FullRangeEncodedDecodedAsLimitedRange,
        );
        assert_eq!(
            ReferenceComparisonVerdict::for_luma_ratio(ratio),
            ReferenceComparisonVerdict::Fail,
            "expanding 16-235 across a full ramp clips both ends; got {ratio:?}"
        );
    }

    /// The half of non-vacuity that is easy to lose: a mode that corrupted
    /// *every* frame would trip the gate for the wrong reason and hide a real
    /// chroma bug behind a guaranteed failure. Each mode is blind to some
    /// content class, and the reference set carries all of them.
    #[test]
    fn each_mode_leaves_the_content_class_it_cannot_see_untouched() {
        let greyscale_ramp = horizontal_luma_ramp_image(256, 8);
        for chroma_only_mode in [
            InjectedColorRegression::SwapRedAndBlueChannels,
            InjectedColorRegression::Bt601EncodedDecodedAsBt709,
        ] {
            assert_eq!(
                greyscale_ramp.with_injected_color_regression(chroma_only_mode),
                greyscale_ramp,
                "{} is a chroma regression and a greyscale frame carries no chroma",
                chroma_only_mode.as_command_line_value()
            );
        }

        // Saturated primaries already sit at the ends of the coded range, so
        // re-expanding them clips straight back to where they were. This is why
        // the vivid rig refuses `range-swap` and the gradients carry it.
        let saturated_red = solid_color_image(16, 16, [255, 0, 0]);
        assert_eq!(
            saturated_red.with_injected_color_regression(
                InjectedColorRegression::FullRangeEncodedDecodedAsLimitedRange
            ),
            saturated_red
        );
    }

    /// The whole non-vacuity claim, on the bytes that actually ship: each mode
    /// is run against a perfect round trip of the checked-in reference set and
    /// must trip *exactly* the references whose content class carries it.
    ///
    /// An exact set rather than "at least one" because both directions are
    /// failures. A mode that trips nothing passes a run carrying that
    /// regression; a mode that trips everything would gate on the injection
    /// itself and could hide a real chroma bug behind a guaranteed failure.
    #[test]
    fn every_injection_mode_trips_exactly_the_references_its_content_class_carries() {
        let reference_directory = std::path::Path::new(CHECKED_IN_REFERENCE_DIRECTORY);
        let mut reference_paths: Vec<std::path::PathBuf> = std::fs::read_dir(reference_directory)
            .expect("the checked-in reference set is part of the repo")
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|extension| extension == "png"))
            .collect();
        reference_paths.sort();
        assert_eq!(
            reference_paths.len(),
            9,
            "the reference set moved; the expectations below are tuned to its content classes \
             (six solids, two greyscale ramps, one detailed pattern) and a changed set has to be \
             re-measured rather than have this test relaxed"
        );

        // Read once: the same reference is scored against three injections.
        let references: Vec<(String, Rgba8Image)> = reference_paths
            .iter()
            .map(|reference_path| {
                (
                    reference_path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    Rgba8Image::read_png(reference_path).unwrap(),
                )
            })
            .collect();

        for (regression, expected_to_trip) in [
            // Green survives an R<->B swap untouched, which is why the set
            // carries all three primaries rather than one.
            (
                InjectedColorRegression::SwapRedAndBlueChannels,
                vec!["complex_pattern", "solid_blue", "solid_red"],
            ),
            (
                InjectedColorRegression::Bt601EncodedDecodedAsBt709,
                vec!["complex_pattern", "solid_blue", "solid_green", "solid_red"],
            ),
            // Only the ramps: saturated primaries and both extremes of grey sit
            // at the ends of the coded range and clip straight back.
            (
                InjectedColorRegression::FullRangeEncodedDecodedAsLimitedRange,
                vec!["gradient_horizontal", "gradient_vertical"],
            ),
        ] {
            let tripped: Vec<&str> = references
                .iter()
                .filter(|(_, reference)| {
                    ReferenceComparisonVerdict::for_luma_ratio(
                        luma_ratio_against_itself_after_injecting(reference, regression),
                    ) == ReferenceComparisonVerdict::Fail
                })
                .map(|(reference_stem, _)| reference_stem.as_str())
                .collect();
            assert_eq!(
                tripped,
                expected_to_trip,
                "`{}` no longer trips the references it is the gate for",
                regression.as_command_line_value()
            );
        }
    }

    #[test]
    fn an_odd_extent_subsamples_chroma_over_only_the_samples_that_exist() {
        // 3x1 leaves the right-hand chroma sample covering one pixel, not four.
        let mut rgba_bytes = Vec::new();
        for red in [255u8, 255, 0] {
            rgba_bytes.extend_from_slice(&[red, 0, 0, 0xFF]);
        }
        let planes = Rgba8Image::from_rgba_bytes(3, 1, rgba_bytes)
            .unwrap()
            .to_bt709_full_range_yuv420_planes();

        assert_eq!(planes.luma_plane.len(), 3);
        assert_eq!(planes.red_difference_chroma_plane.len(), 2);
        // The lone black pixel keeps its own chroma rather than being averaged
        // with samples off the right edge.
        let black_only_chroma = planes.red_difference_chroma_plane[1];
        assert_eq!(
            black_only_chroma, 128,
            "black is chroma-neutral, so an edge sample averaging in phantom \
             neighbours would show here"
        );
    }

    #[test]
    fn a_channel_mean_is_the_normalised_average_of_each_channel() {
        let means = solid_color_image(4, 4, [255, 0, 128]).rgb_channel_means();
        assert!((means.red - 1.0).abs() < 1e-9, "{means:?}");
        assert!(means.green.abs() < 1e-9, "{means:?}");
        assert!((means.blue - 128.0 / 255.0).abs() < 1e-9, "{means:?}");

        let averaged = RgbChannelMeans::averaged(&[
            RgbChannelMeans {
                red: 1.0,
                green: 0.0,
                blue: 0.5,
            },
            RgbChannelMeans {
                red: 0.0,
                green: 1.0,
                blue: 0.5,
            },
        ])
        .unwrap();
        assert!((averaged.red - 0.5).abs() < 1e-9);
        assert!((averaged.green - 0.5).abs() < 1e-9);
        assert!((averaged.blue - 0.5).abs() < 1e-9);
        assert_eq!(RgbChannelMeans::averaged(&[]), None);
    }

    #[test]
    fn an_injection_mode_this_tool_does_not_define_is_refused_by_name() {
        assert!(
            InjectedColorRegression::from_str("gamma-swap", true).is_err(),
            "an unknown mode must exit non-zero rather than run as a no-op"
        );
        for regression in InjectedColorRegression::ALL {
            assert_eq!(
                InjectedColorRegression::from_str(regression.as_command_line_value(), true),
                Ok(regression),
                "the command-line spelling and the parser disagree"
            );
        }
    }

    #[test]
    fn a_greyscale_png_widens_to_rgba_rather_than_being_refused() {
        // Several of the solid fixtures are stored as 1-bit or 8-bit greyscale
        // because that is the narrowest encoding that fits them.
        let scratch = tempfile::tempdir().unwrap();
        let png_path = scratch.path().join("grey.png");
        let file = std::fs::File::create(&png_path).unwrap();
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), 2, 2);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[0, 64, 128, 255])
            .unwrap();

        let read_back = Rgba8Image::read_png(&png_path).unwrap();
        assert_eq!(read_back.pixel_width, 2);
        assert_eq!(read_back.pixel_height, 2);
        assert_eq!(
            read_back.rgba_bytes,
            vec![
                0, 0, 0, 255, //
                64, 64, 64, 255, //
                128, 128, 128, 255, //
                255, 255, 255, 255,
            ]
        );
    }
}
