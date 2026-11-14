// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The image measurements the codec proof scores with: per-plane PSNR against
//! a reference frame, and the per-bar colours the test pattern's drift lock
//! compares to its baseline.
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
/// Derived from six cold rig runs (three per codec, 108 samples): the lowest finite clean
/// chroma figure in the set was `complex_pattern` at 32.23 dB, reproducing to
/// 0.02 dB run-to-run and 0.13 dB across codecs. That derivation predates #2100,
/// which corrected the decode path's chroma siting and moved the same figure to
/// 33.5 dB; `complex_pattern` still carries the minimum and the floor is unchanged.
/// A warn band
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

    /// The median colour of each SMPTE bar, left to right, on the `[0, 1]` scale.
    ///
    /// Read over each bar's middle three quarters of columns, clear of the
    /// chroma a codec bleeds across a bar edge, and the rows from a sixteenth of
    /// the way down to three quarters, above the test pattern's caption panel.
    /// The potato covers less than half of any bar's band, so the median is the
    /// bar's own colour wherever the potato is.
    pub fn smpte_bar_median_colors(&self) -> Result<[RgbOnTheUnitScale; SMPTE_BAR_COUNT]> {
        let first_band_row = self.pixel_height / 16;
        let end_band_row = self.pixel_height * 3 / 4;
        let bar_width = self.pixel_width / SMPTE_BAR_COUNT as u32;
        let bar_edge_inset = bar_width / 8;
        anyhow::ensure!(
            end_band_row > first_band_row && bar_width > 2 * bar_edge_inset,
            "a {}x{} frame is too small to hold eight measurable bars",
            self.pixel_width,
            self.pixel_height
        );

        Ok(std::array::from_fn(|bar_index| {
            let first_bar_column = bar_index as u32 * self.pixel_width / SMPTE_BAR_COUNT as u32;
            let end_bar_column = (bar_index as u32 + 1) * self.pixel_width / SMPTE_BAR_COUNT as u32;
            let mut sample_counts_by_channel = [[0u32; 256]; 3];
            for row in first_band_row..end_band_row {
                for column in (first_bar_column + bar_edge_inset)..(end_bar_column - bar_edge_inset)
                {
                    let offset = ((row * self.pixel_width + column) * 4) as usize;
                    for (channel, sample_counts) in sample_counts_by_channel.iter_mut().enumerate()
                    {
                        sample_counts[usize::from(self.rgba_bytes[offset + channel])] += 1;
                    }
                }
            }
            let [red, green, blue] = sample_counts_by_channel
                .map(|sample_counts| median_sample(&sample_counts) / EIGHT_BIT_SAMPLE_PEAK);
            RgbOnTheUnitScale { red, green, blue }
        }))
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

        for (index, pixel) in self.rgba_bytes.as_chunks::<4>().0.iter().enumerate() {
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
        for pixel in self.rgba_bytes.as_chunks_mut::<4>().0.iter_mut() {
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

/// The SMPTE bars the test pattern draws, left to right, by the names the bar
/// colour baseline keys its rows by.
pub const SMPTE_BAR_NAMES: [&str; SMPTE_BAR_COUNT] = [
    "white", "yellow", "cyan", "green", "magenta", "red", "blue", "black",
];

/// How many bars the test pattern draws across the frame.
pub const SMPTE_BAR_COUNT: usize = 8;

/// The colour the test pattern draws each bar in, left to right, as 8-bit RGB.
pub const SMPTE_BAR_NOMINAL_RGB: [[u8; 3]; SMPTE_BAR_COUNT] = [
    [255, 255, 255],
    [255, 255, 0],
    [0, 255, 255],
    [0, 255, 0],
    [255, 0, 255],
    [255, 0, 0],
    [0, 0, 255],
    [0, 0, 0],
];

/// The lower median of an 8-bit histogram.
fn median_sample(sample_counts: &[u32; 256]) -> f64 {
    let sample_total: u64 = sample_counts.iter().map(|&count| u64::from(count)).sum();
    let median_rank = sample_total.div_ceil(2);
    let mut samples_at_or_below = 0u64;
    for (sample, &count) in sample_counts.iter().enumerate() {
        samples_at_or_below += u64::from(count);
        if samples_at_or_below >= median_rank {
            return sample as f64;
        }
    }
    0.0
}

/// An RGB colour with each channel on the `[0, 1]` scale.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RgbOnTheUnitScale {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
}

impl RgbOnTheUnitScale {
    /// The three channels paired with the single-letter names the baseline TSV
    /// heads its columns with, in the order it writes them.
    pub fn by_baseline_channel_name(&self) -> [(&'static str, f64); 3] {
        [("r", self.red), ("g", self.green), ("b", self.blue)]
    }

    /// The arithmetic mean of a set of per-frame colours — the rig-wide figure
    /// the drift lock compares, so one outlier frame cannot decide a run.
    pub fn averaged(per_frame_colors: &[RgbOnTheUnitScale]) -> Option<RgbOnTheUnitScale> {
        if per_frame_colors.is_empty() {
            return None;
        }
        let frame_count = per_frame_colors.len() as f64;
        Some(RgbOnTheUnitScale {
            red: per_frame_colors.iter().map(|color| color.red).sum::<f64>() / frame_count,
            green: per_frame_colors
                .iter()
                .map(|color| color.green)
                .sum::<f64>()
                / frame_count,
            blue: per_frame_colors.iter().map(|color| color.blue).sum::<f64>() / frame_count,
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
        .as_chunks::<SOURCE_BYTES_PER_PIXEL>()
        .0
        .iter()
        .take(pixel_count)
        .flat_map(|source| widen_one_pixel(*source))
        .collect()
}

#[cfg(test)]
mod tests;
