// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The image measurements the codec proof scores with: per-plane PSNR against
//! a reference frame, and the channel-mean drift lock the vivid colorimetry
//! rig compares to its baseline.
//!
//! Every conversion is BT.709 full-range, applied identically to both sides of
//! a comparison. That is the colour the rig's fixture source declares on every
//! frame it publishes (`ColorInfo { Bt709, Srgb, Full }`), so a score measures
//! the codec round trip rather than a matrix disagreement between the scorer
//! and the pipeline.

use std::borrow::Cow;
use std::path::Path;

use anyhow::{Context, Result};

/// Y-plane PSNR at or above which a round trip is a clean pass, in dB.
pub const LUMA_PSNR_PASS_FLOOR_DB: f64 = 35.0;

/// Y-plane PSNR below which a round trip is a regression, in dB. Between this
/// and [`LUMA_PSNR_PASS_FLOOR_DB`] the result is a warning: visible enough to
/// investigate, not bad enough to have obviously broken.
pub const LUMA_PSNR_WARN_FLOOR_DB: f64 = 30.0;

/// Chroma-plane PSNR below which a round trip is a regression, in dB. One floor
/// for both planes and every reference, and no warn band above it.
///
/// Derived from six cold rig runs (three per codec, 108 samples) recorded in
/// `docs/plan/ARCHITECTURE.md` §Media I/O: the lowest finite clean
/// chroma figure in the set is `complex_pattern` at 32.23 dB, reproducing to
/// 0.02 dB run-to-run and 0.13 dB across codecs. A warn band
/// would be dead space, because a clean chroma figure here is not a quality
/// continuum like luma but a constant of the colour-conversion cascade.
///
/// Which is also the caveat: these are round-trip colour figures, not
/// codec-quality ones. A lossless codec through the same two converters scores
/// within 0.2 dB of a real one.
pub const CHROMA_PSNR_PASS_FLOOR_DB: f64 = 30.0;

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
    pub fn cropped_to(&self, pixel_width: u32, pixel_height: u32) -> Result<Cow<'_, Self>> {
        anyhow::ensure!(
            pixel_width <= self.pixel_width && pixel_height <= self.pixel_height,
            "cannot crop a {}x{} image up to {pixel_width}x{pixel_height}",
            self.pixel_width,
            self.pixel_height
        );
        // The common path: only a CTU-padded decode arrives oversized, so an
        // owning crop here would copy every scored frame for nothing.
        if (pixel_width, pixel_height) == (self.pixel_width, self.pixel_height) {
            return Ok(Cow::Borrowed(self));
        }
        let source_row_byte_len = (self.pixel_width as usize) * 4;
        let cropped_row_byte_len = (pixel_width as usize) * 4;
        let mut rgba_bytes = Vec::with_capacity(cropped_row_byte_len * (pixel_height as usize));
        for row in 0..(pixel_height as usize) {
            let row_start = row * source_row_byte_len;
            rgba_bytes
                .extend_from_slice(&self.rgba_bytes[row_start..row_start + cropped_row_byte_len]);
        }
        Ok(Cow::Owned(Self::from_rgba_bytes(
            pixel_width,
            pixel_height,
            rgba_bytes,
        )?))
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
            luma_plane[index] = round_to_eight_bit_sample(luma);
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
    pub fn with_injected_color_regression(mut self, regression: InjectedColorRegression) -> Self {
        for pixel in self.rgba_bytes.chunks_exact_mut(4) {
            let [red, green, blue] = match regression {
                InjectedColorRegression::SwapRedAndBlueChannels => {
                    [pixel[2], pixel[1], pixel[0]].map(f32::from)
                }
                InjectedColorRegression::Bt601EncodedDecodedAsBt709 => BT709_LUMA_COEFFICIENTS
                    .yuv_to_rgb_full_range(quantized_wire_yuv_of_pixel(
                        &BT601_LUMA_COEFFICIENTS,
                        pixel,
                    )),
                InjectedColorRegression::ChromaPlanesTransposed => {
                    let [luma, blue_difference, red_difference] =
                        quantized_wire_yuv_of_pixel(&BT709_LUMA_COEFFICIENTS, pixel);
                    BT709_LUMA_COEFFICIENTS.yuv_to_rgb_full_range([
                        luma,
                        red_difference,
                        blue_difference,
                    ])
                }
                InjectedColorRegression::FullRangeEncodedDecodedAsLimitedRange => {
                    let [luma, blue_difference, red_difference] =
                        quantized_wire_yuv_of_pixel(&BT709_LUMA_COEFFICIENTS, pixel);
                    BT709_LUMA_COEFFICIENTS.yuv_to_rgb_full_range([
                        expand_limited_range_luma(luma),
                        expand_limited_range_chroma(blue_difference),
                        expand_limited_range_chroma(red_difference),
                    ])
                }
            };
            pixel[0] = round_to_eight_bit_sample(red);
            pixel[1] = round_to_eight_bit_sample(green);
            pixel[2] = round_to_eight_bit_sample(blue);
        }
        self
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
    /// is infinite.
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

/// All three of one decoded frame's plane ratios against its reference.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Yuv420PlanePeakSignalToNoiseRatios {
    pub luma_ratio: PlanePeakSignalToNoiseRatio,
    pub blue_difference_chroma_ratio: PlanePeakSignalToNoiseRatio,
    pub red_difference_chroma_ratio: PlanePeakSignalToNoiseRatio,
}

impl Yuv420PlanePeakSignalToNoiseRatios {
    /// Score a decoded frame's planes against the reference's, plane by plane.
    pub fn between(measured: &Yuv420Planes, reference: &Yuv420Planes) -> Result<Self> {
        Ok(Self {
            luma_ratio: PlanePeakSignalToNoiseRatio::between(
                &measured.luma_plane,
                &reference.luma_plane,
            )?,
            blue_difference_chroma_ratio: PlanePeakSignalToNoiseRatio::between(
                &measured.blue_difference_chroma_plane,
                &reference.blue_difference_chroma_plane,
            )?,
            red_difference_chroma_ratio: PlanePeakSignalToNoiseRatio::between(
                &measured.red_difference_chroma_plane,
                &reference.red_difference_chroma_plane,
            )?,
        })
    }

    /// The two chroma ratios, which share one floor and are interchangeable to
    /// the classification.
    fn chroma_ratios(&self) -> [PlanePeakSignalToNoiseRatio; 2] {
        [
            self.blue_difference_chroma_ratio,
            self.red_difference_chroma_ratio,
        ]
    }

    /// How the frame classifies.
    ///
    /// Luma carries the three-band judgement, and either chroma plane below
    /// [`CHROMA_PSNR_PASS_FLOOR_DB`] fails the frame outright — a chroma
    /// transposition or a plane-offset slip leaves Y at a passing ratio, so a
    /// luma-only verdict would report it as a clean round trip.
    pub fn verdict(&self) -> ReferenceComparisonVerdict {
        if self
            .chroma_ratios()
            .iter()
            .any(|chroma_ratio| !chroma_ratio.reaches_floor_db(CHROMA_PSNR_PASS_FLOOR_DB))
        {
            return ReferenceComparisonVerdict::Fail;
        }
        ReferenceComparisonVerdict::for_luma_ratio(self.luma_ratio)
    }
}

/// How a decoded frame compares to the reference that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceComparisonVerdict {
    /// Y at or above [`LUMA_PSNR_PASS_FLOOR_DB`], both chroma planes at or
    /// above [`CHROMA_PSNR_PASS_FLOOR_DB`].
    Pass,
    /// Y between the warn and pass floors — investigate, do not gate on it.
    /// Chroma has no warn band, so it never lands here.
    Warn,
    /// Y below [`LUMA_PSNR_WARN_FLOOR_DB`], or either chroma plane below
    /// [`CHROMA_PSNR_PASS_FLOOR_DB`]: a colour-matrix, range, or plane-layout
    /// regression.
    Fail,
}

impl ReferenceComparisonVerdict {
    /// The three-band luma judgement, which chroma can only worsen. Private so
    /// that a frame is never classified on luma alone.
    fn for_luma_ratio(luma_ratio: PlanePeakSignalToNoiseRatio) -> Self {
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
    /// Cb and Cr exchanged on the wire — the chroma plane-order class, and the
    /// mode the chroma floor exists for: on `solid_red` and `solid_green` the
    /// luma ratio passes and chroma alone fails the frame. The transposition
    /// leaves Y untouched only where the result stays in gamut, so clamping
    /// drags Y under its own floor on `complex_pattern` and `solid_blue` —
    /// narrow a run to the first two to exercise the chroma floor specifically.
    /// Greyscale-invariant like the other two chroma modes.
    #[value(name = "swap-chroma")]
    ChromaPlanesTransposed,
}

impl InjectedColorRegression {
    /// Every mode, in the order the rig documents them.
    pub const ALL: [InjectedColorRegression; 4] = [
        InjectedColorRegression::SwapRedAndBlueChannels,
        InjectedColorRegression::Bt601EncodedDecodedAsBt709,
        InjectedColorRegression::FullRangeEncodedDecodedAsLimitedRange,
        InjectedColorRegression::ChromaPlanesTransposed,
    ];

    /// The spelling the command line and the fixture scripts use.
    pub fn as_command_line_value(self) -> &'static str {
        match self {
            Self::SwapRedAndBlueChannels => "swap-channels",
            Self::Bt601EncodedDecodedAsBt709 => "bt601-bt709",
            Self::FullRangeEncodedDecodedAsLimitedRange => "range-swap",
            Self::ChromaPlanesTransposed => "swap-chroma",
        }
    }
}

/// Luma weights of one colour matrix, which is the whole difference between
/// BT.601 and BT.709 for these conversions.
///
/// Green is derived, never stored: the inverse below divides by it on the
/// assumption that the three weights sum to one, so a stored green could
/// disagree with the pair it is built from and silently produce a wrong
/// inverse. Same shape as the engine's own
/// `runtime/streamlib-engine/src/core/color/matrix.rs`.
struct LumaCoefficients {
    red: f32,
    blue: f32,
}

impl LumaCoefficients {
    const fn from_red_and_blue(red: f32, blue: f32) -> Self {
        Self { red, blue }
    }

    fn green(&self) -> f32 {
        1.0 - self.red - self.blue
    }
}

const BT709_LUMA_COEFFICIENTS: LumaCoefficients =
    LumaCoefficients::from_red_and_blue(0.2126, 0.0722);

const BT601_LUMA_COEFFICIENTS: LumaCoefficients = LumaCoefficients::from_red_and_blue(0.299, 0.114);

/// Where an 8-bit chroma sample sits when the difference is zero.
const CHROMA_ZERO_LEVEL: f32 = 128.0;

impl LumaCoefficients {
    /// RGB in `[0, 255]` to full-range Y/Cb/Cr, chroma centred on 128.
    ///
    /// Unrounded and unclamped: the 4:2:0 chroma path averages a 2x2 box before
    /// it rounds, and rounding here first would quantise twice.
    fn rgb_to_yuv_full_range(&self, [red, green, blue]: [f32; 3]) -> [f32; 3] {
        let luma = self.red * red + self.green() * green + self.blue * blue;
        [
            luma,
            (blue - luma) / (2.0 * (1.0 - self.blue)) + CHROMA_ZERO_LEVEL,
            (red - luma) / (2.0 * (1.0 - self.red)) + CHROMA_ZERO_LEVEL,
        ]
    }

    /// The inverse of [`Self::rgb_to_yuv_full_range`], unclamped to match it.
    fn yuv_to_rgb_full_range(&self, [luma, blue_difference, red_difference]: [f32; 3]) -> [f32; 3] {
        let blue_difference = blue_difference - CHROMA_ZERO_LEVEL;
        let red_difference = red_difference - CHROMA_ZERO_LEVEL;
        [
            luma + 2.0 * (1.0 - self.red) * red_difference,
            luma - (2.0 * self.red * (1.0 - self.red) / self.green()) * red_difference
                - (2.0 * self.blue * (1.0 - self.blue) / self.green()) * blue_difference,
            luma + 2.0 * (1.0 - self.blue) * blue_difference,
        ]
    }
}

/// Round to the nearest 8-bit sample, saturating at both ends.
fn round_to_eight_bit_sample(sample: f32) -> u8 {
    sample.round().clamp(0.0, EIGHT_BIT_SAMPLE_PEAK as f32) as u8
}

/// Send a YUV triple through the 8-bit quantisation a real bitstream imposes.
///
/// The injected regressions are round trips through a wire, so the intermediate
/// has to be 8-bit: `solid_red` clips its Cr at 255 there, and a float-only
/// chain would score a bug the pipeline could not actually produce.
fn quantized_to_eight_bit_wire_samples(yuv: [f32; 3]) -> [f32; 3] {
    yuv.map(|sample| f32::from(round_to_eight_bit_sample(sample)))
}

/// One RGBA pixel's colour taken through `encoding_coefficients` to the 8-bit
/// YUV a bitstream would carry it as. The shared half of every injection mode
/// that models a round trip through a wire; each mode differs only in what it
/// does with the triple afterwards.
fn quantized_wire_yuv_of_pixel(encoding_coefficients: &LumaCoefficients, pixel: &[u8]) -> [f32; 3] {
    quantized_to_eight_bit_wire_samples(encoding_coefficients.rgb_to_yuv_full_range([
        f32::from(pixel[0]),
        f32::from(pixel[1]),
        f32::from(pixel[2]),
    ]))
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
                round_to_eight_bit_sample(total / sample_count);
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

    fn plane_ratios_against_itself_after_injecting(
        image: &Rgba8Image,
        regression: InjectedColorRegression,
    ) -> Yuv420PlanePeakSignalToNoiseRatios {
        let reference_planes = image.to_bt709_full_range_yuv420_planes();
        let injected_planes = image
            .clone()
            .with_injected_color_regression(regression)
            .to_bt709_full_range_yuv420_planes();
        Yuv420PlanePeakSignalToNoiseRatios::between(&injected_planes, &reference_planes).unwrap()
    }

    /// A frame that round-tripped byte-identically on every plane. The base a
    /// classification test spreads to move one named plane at a time.
    fn perfectly_round_tripped_plane_ratios() -> Yuv420PlanePeakSignalToNoiseRatios {
        Yuv420PlanePeakSignalToNoiseRatios {
            luma_ratio: PlanePeakSignalToNoiseRatio::Identical,
            blue_difference_chroma_ratio: PlanePeakSignalToNoiseRatio::Identical,
            red_difference_chroma_ratio: PlanePeakSignalToNoiseRatio::Identical,
        }
    }

    fn plane_ratios_with_luma_at(decibels: f64) -> Yuv420PlanePeakSignalToNoiseRatios {
        Yuv420PlanePeakSignalToNoiseRatios {
            luma_ratio: PlanePeakSignalToNoiseRatio::Decibels(decibels),
            ..perfectly_round_tripped_plane_ratios()
        }
    }

    fn plane_ratios_with_blue_difference_chroma_at(
        decibels: f64,
    ) -> Yuv420PlanePeakSignalToNoiseRatios {
        Yuv420PlanePeakSignalToNoiseRatios {
            blue_difference_chroma_ratio: PlanePeakSignalToNoiseRatio::Decibels(decibels),
            ..perfectly_round_tripped_plane_ratios()
        }
    }

    fn plane_ratios_with_red_difference_chroma_at(
        decibels: f64,
    ) -> Yuv420PlanePeakSignalToNoiseRatios {
        Yuv420PlanePeakSignalToNoiseRatios {
            red_difference_chroma_ratio: PlanePeakSignalToNoiseRatio::Decibels(decibels),
            ..perfectly_round_tripped_plane_ratios()
        }
    }

    /// The module's whole premise is that the scorer and the pipeline agree on
    /// BT.709 full range. That agreement is derived here from Kr/Kb rather than
    /// written down, so it is pinned against the same canonical numbers the
    /// engine's own `core::color::matrix` test asserts — otherwise a drift in
    /// either would read as a codec regression.
    #[test]
    fn the_derived_bt709_inverse_matches_the_engines_canonical_coefficients() {
        // The engine pins [1.0, 0.0, 1.5748, 1.0, -0.1873, -0.4681, 1.0, 1.8556, 0.0]
        // in `bt709_full_range_matches_canonical_coefficients`.
        let unit_red_difference = BT709_LUMA_COEFFICIENTS.yuv_to_rgb_full_range([
            0.0,
            CHROMA_ZERO_LEVEL,
            CHROMA_ZERO_LEVEL + 1.0,
        ]);
        let unit_blue_difference = BT709_LUMA_COEFFICIENTS.yuv_to_rgb_full_range([
            0.0,
            CHROMA_ZERO_LEVEL + 1.0,
            CHROMA_ZERO_LEVEL,
        ]);

        for (derived, canonical, name) in [
            (unit_red_difference[0], 1.5748, "R from Cr"),
            (unit_red_difference[1], -0.4681, "G from Cr"),
            (unit_blue_difference[1], -0.1873, "G from Cb"),
            (unit_blue_difference[2], 1.8556, "B from Cb"),
        ] {
            assert!(
                (derived - canonical).abs() < 1e-4,
                "{name}: derived {derived}, canonical {canonical}"
            );
        }
        assert!(unit_red_difference[2].abs() < 1e-6, "Cr does not reach B");
        assert!(unit_blue_difference[0].abs() < 1e-6, "Cb does not reach R");
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
        use ReferenceComparisonVerdict::{Fail, Pass, Warn};
        for (plane_ratios, expected) in [
            (perfectly_round_tripped_plane_ratios(), Pass),
            (plane_ratios_with_luma_at(LUMA_PSNR_PASS_FLOOR_DB), Pass),
            (
                plane_ratios_with_luma_at(LUMA_PSNR_PASS_FLOOR_DB - 0.01),
                Warn,
            ),
            (plane_ratios_with_luma_at(LUMA_PSNR_WARN_FLOOR_DB), Warn),
            (
                plane_ratios_with_luma_at(LUMA_PSNR_WARN_FLOOR_DB - 0.01),
                Fail,
            ),
            (
                plane_ratios_with_blue_difference_chroma_at(CHROMA_PSNR_PASS_FLOOR_DB),
                Pass,
            ),
            (
                plane_ratios_with_blue_difference_chroma_at(CHROMA_PSNR_PASS_FLOOR_DB - 0.01),
                Fail,
            ),
            (
                plane_ratios_with_red_difference_chroma_at(CHROMA_PSNR_PASS_FLOOR_DB - 0.01),
                Fail,
            ),
        ] {
            assert_eq!(
                plane_ratios.verdict(),
                expected,
                "{plane_ratios:?} classified wrong"
            );
        }
    }

    /// The whole point of the chroma floor: chroma is judged on its own, not
    /// as a tie-break on a luma figure that already decided the frame.
    #[test]
    fn a_chroma_plane_under_its_floor_fails_a_frame_whose_luma_passes_comfortably() {
        assert_eq!(
            Yuv420PlanePeakSignalToNoiseRatios {
                luma_ratio: PlanePeakSignalToNoiseRatio::Decibels(48.0),
                ..plane_ratios_with_red_difference_chroma_at(24.0)
            }
            .verdict(),
            ReferenceComparisonVerdict::Fail,
            "a Cr plane 24 dB down is a regression however clean Y is"
        );
        // The floor cannot be satisfied by averaging the two chroma planes
        // together: one healthy plane does not cover for the other.
        assert_eq!(
            Yuv420PlanePeakSignalToNoiseRatios {
                blue_difference_chroma_ratio: PlanePeakSignalToNoiseRatio::Decibels(60.0),
                ..plane_ratios_with_red_difference_chroma_at(24.0)
            }
            .verdict(),
            ReferenceComparisonVerdict::Fail
        );
        // Chroma has no warn band, so a chroma-clean frame keeps luma's.
        assert_eq!(
            plane_ratios_with_luma_at(32.0).verdict(),
            ReferenceComparisonVerdict::Warn
        );
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

        assert_eq!(decoded.cropped_to(8, 4).unwrap().into_owned(), reference);
        assert!(
            decoded.cropped_to(8, 8).is_err(),
            "cropping up would invent rows nobody decoded"
        );
    }

    #[test]
    fn swapping_red_and_blue_moves_a_saturated_frame_below_the_fail_floor() {
        let plane_ratios = plane_ratios_against_itself_after_injecting(
            &solid_color_image(16, 16, [255, 0, 0]),
            InjectedColorRegression::SwapRedAndBlueChannels,
        );
        assert_eq!(
            plane_ratios.verdict(),
            ReferenceComparisonVerdict::Fail,
            "R<->B on a saturated red must trip the gate; got {plane_ratios:?}"
        );
    }

    #[test]
    fn the_bt601_bt709_mismatch_moves_a_saturated_frame_below_the_fail_floor() {
        let plane_ratios = plane_ratios_against_itself_after_injecting(
            &solid_color_image(16, 16, [255, 0, 0]),
            InjectedColorRegression::Bt601EncodedDecodedAsBt709,
        );
        assert_eq!(
            plane_ratios.verdict(),
            ReferenceComparisonVerdict::Fail,
            "a matrix mis-interpretation on a saturated red must trip the gate; \
             got {plane_ratios:?}"
        );
    }

    #[test]
    fn the_range_swap_moves_a_luma_ramp_below_the_fail_floor() {
        let plane_ratios = plane_ratios_against_itself_after_injecting(
            &horizontal_luma_ramp_image(256, 8),
            InjectedColorRegression::FullRangeEncodedDecodedAsLimitedRange,
        );
        assert_eq!(
            plane_ratios.verdict(),
            ReferenceComparisonVerdict::Fail,
            "expanding 16-235 across a full ramp clips both ends; got {plane_ratios:?}"
        );
        assert_eq!(
            ReferenceComparisonVerdict::for_luma_ratio(plane_ratios.luma_ratio),
            ReferenceComparisonVerdict::Fail,
            "the range swap is a luma regression, and must trip the gate as one"
        );
    }

    #[test]
    fn a_chroma_transposition_is_caught_by_chroma_where_the_luma_gate_passes_it() {
        let plane_ratios = plane_ratios_against_itself_after_injecting(
            &solid_color_image(16, 16, [255, 0, 0]),
            InjectedColorRegression::ChromaPlanesTransposed,
        );
        assert_eq!(
            ReferenceComparisonVerdict::for_luma_ratio(plane_ratios.luma_ratio),
            ReferenceComparisonVerdict::Pass,
            "if luma caught this the test would prove nothing about chroma; \
             got {plane_ratios:?}"
        );
        assert_eq!(
            plane_ratios.verdict(),
            ReferenceComparisonVerdict::Fail,
            "a total chroma inversion must fail the frame; got {plane_ratios:?}"
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
            InjectedColorRegression::ChromaPlanesTransposed,
        ] {
            assert_eq!(
                greyscale_ramp
                    .clone()
                    .with_injected_color_regression(chroma_only_mode),
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
            saturated_red.clone().with_injected_color_regression(
                InjectedColorRegression::FullRangeEncodedDecodedAsLimitedRange
            ),
            saturated_red
        );
    }

    /// The checked-in reference set, by stem, in sorted order. Read as a whole
    /// because each caller's expectations are tuned to the content classes the
    /// set carries, so a changed set has to be re-measured rather than have a
    /// test relaxed around it.
    fn sorted_checked_in_references() -> Vec<(String, Rgba8Image)> {
        let mut reference_paths: Vec<std::path::PathBuf> =
            std::fs::read_dir(std::path::Path::new(CHECKED_IN_REFERENCE_DIRECTORY))
                .expect("the checked-in reference set is part of the repo")
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.extension().is_some_and(|extension| extension == "png"))
                .collect();
        reference_paths.sort();
        assert_eq!(
            reference_paths.len(),
            9,
            "the reference set moved: six solids, two greyscale ramps, one detailed pattern. \
             CHROMA_PSNR_PASS_FLOOR_DB was derived against this set — a reference carrying more \
             saturated chroma detail than `complex_pattern` needs the floor re-measured, not \
             this assertion relaxed"
        );
        reference_paths
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
            .collect()
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
        // Read once: the same reference is scored against every injection.
        let references = sorted_checked_in_references();

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
            // Every reference carrying chroma at all. Two of them —
            // `solid_green` and `solid_red` — trip on chroma while their luma
            // ratio passes, which is what the next assertion pins.
            (
                InjectedColorRegression::ChromaPlanesTransposed,
                vec!["complex_pattern", "solid_blue", "solid_green", "solid_red"],
            ),
        ] {
            let tripped: Vec<&str> = references
                .iter()
                .filter(|(_, reference)| {
                    plane_ratios_against_itself_after_injecting(reference, regression).verdict()
                        == ReferenceComparisonVerdict::Fail
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

    /// Each mode's effect on chroma *alone*, with luma set aside entirely — the
    /// verdict sets above cannot tell a chroma trip from luma collateral, and
    /// three of these modes are chroma regressions whose whole claim is that
    /// they move U and V.
    ///
    /// `range-swap` mapping to the empty set is the load-bearing negative: it
    /// is a luma regression, and a chroma floor that fired on it would be
    /// reading luma damage through the wrong plane.
    #[test]
    fn the_chroma_floor_fires_on_exactly_the_modes_that_are_chroma_regressions() {
        let references = sorted_checked_in_references();

        for (regression, expected_to_trip_on_chroma) in [
            (
                InjectedColorRegression::SwapRedAndBlueChannels,
                vec!["complex_pattern", "solid_blue", "solid_red"],
            ),
            // `solid_blue` is absent: the matrix error puts its Cb at 34.15 dB
            // and its Cr at 31.23 dB, both over the floor. Its luma catches it.
            (
                InjectedColorRegression::Bt601EncodedDecodedAsBt709,
                vec!["complex_pattern", "solid_green", "solid_red"],
            ),
            (
                InjectedColorRegression::FullRangeEncodedDecodedAsLimitedRange,
                vec![],
            ),
            (
                InjectedColorRegression::ChromaPlanesTransposed,
                vec!["complex_pattern", "solid_blue", "solid_green", "solid_red"],
            ),
        ] {
            let tripped_on_chroma: Vec<&str> = references
                .iter()
                .filter(|(_, reference)| {
                    let plane_ratios =
                        plane_ratios_against_itself_after_injecting(reference, regression);
                    [
                        plane_ratios.blue_difference_chroma_ratio,
                        plane_ratios.red_difference_chroma_ratio,
                    ]
                    .iter()
                    .any(|chroma_ratio| !chroma_ratio.reaches_floor_db(CHROMA_PSNR_PASS_FLOOR_DB))
                })
                .map(|(reference_stem, _)| reference_stem.as_str())
                .collect();
            assert_eq!(
                tripped_on_chroma,
                expected_to_trip_on_chroma,
                "`{}` no longer moves chroma the way its class says it does",
                regression.as_command_line_value()
            );
        }
    }

    /// Non-vacuity of the chroma floor itself, on the bytes that ship. The
    /// three older modes are all caught by luma as well, so the floor would be
    /// pure decoration without a reference that only chroma catches.
    #[test]
    fn the_chroma_floor_catches_references_the_luma_gate_passes() {
        let caught_by_chroma_alone: Vec<String> = sorted_checked_in_references()
            .into_iter()
            .filter_map(|(reference_stem, reference)| {
                let plane_ratios = plane_ratios_against_itself_after_injecting(
                    &reference,
                    InjectedColorRegression::ChromaPlanesTransposed,
                );
                let caught_by_luma_too =
                    ReferenceComparisonVerdict::for_luma_ratio(plane_ratios.luma_ratio)
                        == ReferenceComparisonVerdict::Fail;
                (plane_ratios.verdict() == ReferenceComparisonVerdict::Fail && !caught_by_luma_too)
                    .then_some(reference_stem)
            })
            .collect();

        assert_eq!(
            caught_by_chroma_alone,
            ["solid_green", "solid_red"],
            "a chroma floor that never catches what luma misses gates nothing"
        );
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
