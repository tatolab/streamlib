// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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

fn plane_ratios_with_red_difference_chroma_at(decibels: f64) -> Yuv420PlanePeakSignalToNoiseRatios {
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
    // re-expanding them clips straight back to where they were.
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

/// Eight vertical bars in the test pattern's colours, a dark square
/// covering some of `covered_bar` the way the potato does.
fn smpte_bars_image_with_a_square_over(
    pixel_width: u32,
    pixel_height: u32,
    covered_bar: usize,
    square_side: u32,
) -> Rgba8Image {
    let square_left = covered_bar as u32 * pixel_width / SMPTE_BAR_COUNT as u32;
    let square_top = pixel_height / 8;
    let mut rgba_bytes = Vec::with_capacity((pixel_width * pixel_height * 4) as usize);
    for row in 0..pixel_height {
        for column in 0..pixel_width {
            let inside_the_square = (square_left..square_left + square_side).contains(&column)
                && (square_top..square_top + square_side).contains(&row);
            let [red, green, blue] = if inside_the_square {
                [90, 60, 30]
            } else {
                SMPTE_BAR_NOMINAL_RGB[(column as usize * SMPTE_BAR_COUNT / pixel_width as usize)
                    .min(SMPTE_BAR_COUNT - 1)]
            };
            rgba_bytes.extend_from_slice(&[red, green, blue, 0xFF]);
        }
    }
    Rgba8Image::from_rgba_bytes(pixel_width, pixel_height, rgba_bytes).unwrap()
}

#[test]
fn each_bar_measures_its_own_colour_with_the_potato_over_it() {
    // A 1280x720 frame's bar is 160 px wide, as wide as the potato; the
    // square covers it whole for 160 of the band's 495 rows.
    let bar_colors = smpte_bars_image_with_a_square_over(1280, 720, 5, 160)
        .smpte_bar_median_colors()
        .unwrap();

    let as_bytes = |color: RgbOnTheUnitScale| {
        [color.red, color.green, color.blue].map(|channel| (channel * 255.0).round() as u8)
    };
    assert_eq!(as_bytes(bar_colors[0]), [255, 255, 255], "white");
    assert_eq!(as_bytes(bar_colors[2]), [0, 255, 255], "cyan");
    assert_eq!(as_bytes(bar_colors[5]), [255, 0, 0], "red under the square");
    assert_eq!(as_bytes(bar_colors[7]), [0, 0, 0], "black");
}

#[test]
fn every_injected_regression_drifts_a_bar_past_the_default_tolerance() {
    let clean = smpte_bars_image_with_a_square_over(1280, 720, 3, 160);
    let clean_bar_colors = clean.smpte_bar_median_colors().unwrap();
    for regression in [
        InjectedColorRegression::SwapRedAndBlueChannels,
        InjectedColorRegression::Bt601EncodedDecodedAsBt709,
        InjectedColorRegression::ChromaPlanesTransposed,
    ] {
        let injected_bar_colors = clean
            .clone()
            .with_injected_color_regression(regression)
            .smpte_bar_median_colors()
            .unwrap();
        let largest_drift = clean_bar_colors
            .iter()
            .zip(&injected_bar_colors)
            .flat_map(|(clean_color, injected_color)| {
                clean_color
                    .by_baseline_channel_name()
                    .into_iter()
                    .zip(injected_color.by_baseline_channel_name())
                    .map(|((_, clean_channel), (_, injected_channel))| {
                        (clean_channel - injected_channel).abs()
                    })
            })
            .fold(0.0, f64::max);
        assert!(
            largest_drift > 0.05,
            "{} drifts no bar past 0.05 (largest {largest_drift:.4}) — the lock would be \
                 vacuous for it",
            regression.as_command_line_value()
        );
    }
}

#[test]
fn a_frame_too_small_for_eight_bars_is_refused_rather_than_measured_as_black() {
    assert!(
        solid_color_image(4, 4, [255, 0, 0])
            .smpte_bar_median_colors()
            .is_err()
    );
}

#[test]
fn per_frame_colours_average_channel_by_channel() {
    let averaged = RgbOnTheUnitScale::averaged(&[
        RgbOnTheUnitScale {
            red: 1.0,
            green: 0.0,
            blue: 0.5,
        },
        RgbOnTheUnitScale {
            red: 0.0,
            green: 1.0,
            blue: 0.5,
        },
    ])
    .unwrap();
    assert!((averaged.red - 0.5).abs() < 1e-9);
    assert!((averaged.green - 0.5).abs() < 1e-9);
    assert!((averaged.blue - 0.5).abs() < 1e-9);
    assert_eq!(RgbOnTheUnitScale::averaged(&[]), None);
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
