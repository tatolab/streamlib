// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `cargo xtask psnr` — the codec proof's scorer.
//!
//! Two verbs over one set of measurements ([`crate::codec_proof_image_measurement`]):
//! `score` compares a decoded frame set to the references that produced it, and
//! `bar-colors` is the test pattern's colour drift lock.
//!
//! Frames reach this tool as PNGs on disk, which the rig's ffmpeg decoded from
//! an encoded track `tatolab-read-track` read at a relay. Nothing here touches a
//! GPU.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Subcommand;

use crate::codec_proof_image_measurement::{
    CHROMA_PSNR_PASS_FLOOR_DB, InjectedColorRegression, LUMA_PSNR_PASS_FLOOR_DB,
    LUMA_PSNR_WARN_FLOOR_DB, ReferenceComparisonVerdict, RgbOnTheUnitScale, Rgba8Image,
    SMPTE_BAR_COUNT, SMPTE_BAR_NAMES, Yuv420PlanePeakSignalToNoiseRatios,
};

/// What separates a decoded sample's reference stem from the rest of its file
/// name: `solid_red__0.png` scores against `solid_red.png`. The fixture script
/// names the files, and this is the whole pairing contract between them.
const DECODED_SAMPLE_REFERENCE_STEM_SEPARATOR: &str = "__";

/// Default absolute drift a bar's channel may carry from its baseline, on the
/// `[0, 1]` scale.
const DEFAULT_BAR_COLOR_DRIFT_TOLERANCE: f64 = 0.05;

/// Each SMPTE bar's colour, in the bars' left-to-right order.
type SmpteBarColors = [RgbOnTheUnitScale; SMPTE_BAR_COUNT];

#[derive(Subcommand)]
pub enum PsnrCommand {
    /// Score decoded frames against the reference PNGs that produced them:
    /// per-plane Y/U/V PSNR, classified Y >= 35 dB pass / 30-35 warn / < 30
    /// fail, and either chroma plane under 30 dB failing outright. Exits
    /// non-zero when any reference fails or went unsampled.
    Score {
        /// Directory of decoded PNGs, each named `<reference_stem>__<n>.png`.
        #[arg(long)]
        decoded: PathBuf,
        /// Directory of reference PNGs.
        #[arg(long)]
        reference: PathBuf,
        /// Corrupt every decoded frame with a known colour-management
        /// regression before scoring, to prove the gate is live.
        #[arg(long)]
        inject: Option<InjectedColorRegression>,
        /// Write the per-pair table as TSV.
        #[arg(long)]
        report: Option<PathBuf>,
    },

    /// The test pattern's colour drift lock: each SMPTE bar's median colour,
    /// averaged across a decoded frame set, compared to a checked-in baseline
    /// TSV. Each bar is a saturated primary, secondary or neutral, so a matrix
    /// mis-interpretation moves the bars it touches by more than a whole-frame
    /// average would show.
    BarColors {
        /// Directory of decoded PNGs.
        #[arg(long)]
        images: PathBuf,
        /// Baseline TSV: compared against, or overwritten with
        /// `--capture-baseline`.
        #[arg(long)]
        baseline: PathBuf,
        /// Corrupt every frame with a known colour-management regression
        /// before measuring.
        #[arg(long)]
        inject: Option<InjectedColorRegression>,
        /// Absolute drift a bar's channel may carry, on the `[0, 1]` scale.
        #[arg(long, default_value_t = DEFAULT_BAR_COLOR_DRIFT_TOLERANCE)]
        tolerance: f64,
        /// Overwrite the baseline with this run's colours instead of comparing.
        #[arg(long)]
        capture_baseline: bool,
        /// Provenance lines recorded in a captured baseline's header, repeatable
        /// — the codec and what the numbers were measured from, which is what
        /// makes them re-derivable rather than just re-checkable.
        #[arg(long)]
        baseline_note: Vec<String>,
        /// Write each frame's bar colours as TSV.
        #[arg(long)]
        report: Option<PathBuf>,
    },
}

pub fn run(command: PsnrCommand) -> Result<()> {
    match command {
        PsnrCommand::Score {
            decoded,
            reference,
            inject,
            report,
        } => {
            score_decoded_frames_against_references(&decoded, &reference, inject, report.as_deref())
        }
        PsnrCommand::BarColors {
            images,
            baseline,
            inject,
            tolerance,
            capture_baseline,
            baseline_note,
            report,
        } => {
            let measured = measure_bar_colors(&images, inject, report.as_deref())?;
            if capture_baseline {
                write_bar_color_baseline(&baseline, &measured, &baseline_note)
            } else {
                compare_bar_colors_to_baseline(&baseline, &measured, tolerance)
            }
        }
    }
}

/// One decoded sample scored against its reference.
struct ScoredReferenceComparison {
    reference_stem: String,
    decoded_file_name: String,
    plane_ratios: Yuv420PlanePeakSignalToNoiseRatios,
}

fn score_decoded_frames_against_references(
    decoded_directory: &Path,
    reference_directory: &Path,
    inject: Option<InjectedColorRegression>,
    report_path: Option<&Path>,
) -> Result<()> {
    let reference_paths = sorted_png_paths_in(reference_directory)?;
    anyhow::ensure!(
        !reference_paths.is_empty(),
        "no reference PNGs in {}",
        reference_directory.display()
    );
    let decoded_paths = sorted_png_paths_in(decoded_directory)?;
    anyhow::ensure!(
        !decoded_paths.is_empty(),
        "no decoded PNGs in {} — the run captured nothing to score",
        decoded_directory.display()
    );

    let mut references_by_stem: BTreeMap<String, Rgba8Image> = BTreeMap::new();
    for reference_path in &reference_paths {
        references_by_stem.insert(
            file_stem_of(reference_path)?,
            Rgba8Image::read_png(reference_path)?,
        );
    }

    if let Some(regression) = inject {
        tracing::warn!(
            "INJECTING {} into every decoded frame — this run is expected to FAIL",
            regression.as_command_line_value()
        );
    }

    let mut scored_comparisons: Vec<ScoredReferenceComparison> = Vec::new();
    for decoded_path in &decoded_paths {
        let decoded_file_name = file_stem_of(decoded_path)?;
        let reference_stem = decoded_file_name
            .split(DECODED_SAMPLE_REFERENCE_STEM_SEPARATOR)
            .next()
            .unwrap_or_default()
            .to_string();
        let reference = references_by_stem.get(&reference_stem).with_context(|| {
            format!(
                "{} names reference `{reference_stem}`, which is not in {}. A decoded sample is \
                 named `<reference_stem>{DECODED_SAMPLE_REFERENCE_STEM_SEPARATOR}<n>.png`; \
                 scoring it against a reference it does not name would measure the wrong pair.",
                decoded_path.display(),
                reference_directory.display()
            )
        })?;

        let mut decoded = Rgba8Image::read_png(decoded_path)?;
        if let Some(regression) = inject {
            decoded = decoded.with_injected_color_regression(regression);
        }
        scored_comparisons.push(score_one_pair(
            reference_stem,
            decoded_file_name,
            &decoded,
            reference,
        )?);
    }

    let sampled_reference_stems: std::collections::BTreeSet<&str> = scored_comparisons
        .iter()
        .map(|comparison| comparison.reference_stem.as_str())
        .collect();
    let unsampled_reference_stems: Vec<&str> = references_by_stem
        .keys()
        .map(String::as_str)
        .filter(|stem| !sampled_reference_stems.contains(stem))
        .collect();

    report_scored_comparisons(&scored_comparisons, &unsampled_reference_stems, report_path)?;

    let failed_sample_names: Vec<&str> = scored_comparisons
        .iter()
        .filter(|comparison| comparison.plane_ratios.verdict() == ReferenceComparisonVerdict::Fail)
        .map(|comparison| comparison.decoded_file_name.as_str())
        .collect();
    anyhow::ensure!(
        failed_sample_names.is_empty() && unsampled_reference_stems.is_empty(),
        "PSNR gate FAILED — {} sample(s) below {LUMA_PSNR_WARN_FLOOR_DB} dB Y or \
         {CHROMA_PSNR_PASS_FLOOR_DB} dB chroma ({}), {} reference(s) never sampled ({})",
        failed_sample_names.len(),
        comma_joined_or_none(&failed_sample_names),
        unsampled_reference_stems.len(),
        comma_joined_or_none(&unsampled_reference_stems)
    );
    tracing::info!("[psnr] RESULT: PASS");
    Ok(())
}

/// Score one decoded frame against its reference, cropping the decoded side to
/// the reference extent first — a CTU-padded H.265 decode arrives 1920x1088
/// for a 1920x1080 picture and the padding is not part of the comparison.
fn score_one_pair(
    reference_stem: String,
    decoded_file_name: String,
    decoded: &Rgba8Image,
    reference: &Rgba8Image,
) -> Result<ScoredReferenceComparison> {
    let cropped = decoded
        .cropped_to(reference.pixel_width, reference.pixel_height)
        .with_context(|| {
            format!(
                "decoded sample `{decoded_file_name}` is {}x{}, smaller than its {}x{} reference",
                decoded.pixel_width,
                decoded.pixel_height,
                reference.pixel_width,
                reference.pixel_height
            )
        })?;

    let plane_ratios = Yuv420PlanePeakSignalToNoiseRatios::between(
        &cropped.to_bt709_full_range_yuv420_planes(),
        &reference.to_bt709_full_range_yuv420_planes(),
    )?;

    Ok(ScoredReferenceComparison {
        reference_stem,
        decoded_file_name,
        plane_ratios,
    })
}

/// Names for a failure message, or the word that says there were none — so an
/// empty list never reads as an omission.
fn comma_joined_or_none(names: &[&str]) -> String {
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    }
}

fn report_scored_comparisons(
    scored_comparisons: &[ScoredReferenceComparison],
    unsampled_reference_stems: &[&str],
    report_path: Option<&Path>,
) -> Result<()> {
    tracing::info!("══════════════════════════════════════════════════════════════════");
    tracing::info!(
        "  Fixture PSNR (Y >= {LUMA_PSNR_PASS_FLOOR_DB} pass, \
         {LUMA_PSNR_WARN_FLOOR_DB}-{LUMA_PSNR_PASS_FLOOR_DB} warn, \
         < {LUMA_PSNR_WARN_FLOOR_DB} fail; U and V >= {CHROMA_PSNR_PASS_FLOOR_DB} or fail)"
    );
    tracing::info!("══════════════════════════════════════════════════════════════════");
    tracing::info!(
        "  {:<28}  {:>8}  {:>8}  {:>8}   {}",
        "decoded sample",
        "Y(dB)",
        "U(dB)",
        "V(dB)",
        "verdict"
    );

    let mut report_tsv = String::from("reference\tdecoded_sample\ty_db\tu_db\tv_db\tverdict\n");
    for comparison in scored_comparisons {
        let plane_ratios = comparison.plane_ratios;
        let verdict = plane_ratios.verdict();
        tracing::info!(
            "  {:<28}  {:>8}  {:>8}  {:>8}   {}",
            comparison.decoded_file_name,
            plane_ratios.luma_ratio.as_report_column(),
            plane_ratios.blue_difference_chroma_ratio.as_report_column(),
            plane_ratios.red_difference_chroma_ratio.as_report_column(),
            verdict.as_report_column()
        );
        writeln!(
            report_tsv,
            "{}\t{}\t{}\t{}\t{}\t{}",
            comparison.reference_stem,
            comparison.decoded_file_name,
            plane_ratios.luma_ratio.as_report_column(),
            plane_ratios.blue_difference_chroma_ratio.as_report_column(),
            plane_ratios.red_difference_chroma_ratio.as_report_column(),
            verdict.as_report_column()
        )?;
    }
    for unsampled_reference_stem in unsampled_reference_stems {
        tracing::error!(
            "  {:<28}  {:>8}  {:>8}  {:>8}   {}",
            unsampled_reference_stem,
            "n/a",
            "n/a",
            "n/a",
            "NO-SAMPLE"
        );
        writeln!(
            report_tsv,
            "{unsampled_reference_stem}\t-\tn/a\tn/a\tn/a\tNO-SAMPLE"
        )?;
    }
    tracing::info!("══════════════════════════════════════════════════════════════════");

    if let Some(report_path) = report_path {
        write_file_creating_parents(report_path, &report_tsv)?;
        tracing::info!("  Report TSV: {}", report_path.display());
    }
    Ok(())
}

fn measure_bar_colors(
    images_directory: &Path,
    inject: Option<InjectedColorRegression>,
    report_path: Option<&Path>,
) -> Result<SmpteBarColors> {
    let image_paths = sorted_png_paths_in(images_directory)?;
    anyhow::ensure!(
        !image_paths.is_empty(),
        "no PNGs in {} — the run captured nothing to measure",
        images_directory.display()
    );
    if let Some(regression) = inject {
        tracing::warn!(
            "INJECTING {} into every frame — this run is expected to drift",
            regression.as_command_line_value()
        );
    }

    let mut report_tsv = String::from("sample\tbar\tr\tg\tb\n");
    let mut per_frame_colors_by_bar: [Vec<RgbOnTheUnitScale>; SMPTE_BAR_COUNT] =
        std::array::from_fn(|_| Vec::with_capacity(image_paths.len()));
    for image_path in &image_paths {
        let mut image = Rgba8Image::read_png(image_path)?;
        if let Some(regression) = inject {
            image = image.with_injected_color_regression(regression);
        }
        let bar_colors = image
            .smpte_bar_median_colors()
            .with_context(|| format!("measuring the bars of {}", image_path.display()))?;
        let sample_name = image_path.file_name().unwrap_or_default().to_string_lossy();
        for ((bar_name, bar_color), per_frame_colors) in SMPTE_BAR_NAMES
            .iter()
            .zip(bar_colors)
            .zip(per_frame_colors_by_bar.iter_mut())
        {
            writeln!(
                report_tsv,
                "{sample_name}\t{bar_name}\t{:.6}\t{:.6}\t{:.6}",
                bar_color.red, bar_color.green, bar_color.blue
            )?;
            per_frame_colors.push(bar_color);
        }
    }

    let rig_wide_bar_colors: SmpteBarColors = per_frame_colors_by_bar.map(|per_frame_colors| {
        RgbOnTheUnitScale::averaged(&per_frame_colors)
            .expect("the image list was checked non-empty above")
    });
    tracing::info!("══════════════════════════════════════════════════════════════════");
    tracing::info!("  Test-pattern colour round trip: each bar's median colour");
    tracing::info!("══════════════════════════════════════════════════════════════════");
    tracing::info!("  Samples:  {}", image_paths.len());
    for (bar_name, bar_color) in SMPTE_BAR_NAMES.iter().zip(&rig_wide_bar_colors) {
        tracing::info!(
            "  {bar_name:<8} R {:.4}  G {:.4}  B {:.4}",
            bar_color.red,
            bar_color.green,
            bar_color.blue
        );
    }

    if let Some(report_path) = report_path {
        write_file_creating_parents(report_path, &report_tsv)?;
        tracing::info!("  Per-sample stats: {}", report_path.display());
    }
    Ok(rig_wide_bar_colors)
}

fn compare_bar_colors_to_baseline(
    baseline_path: &Path,
    measured: &SmpteBarColors,
    tolerance: f64,
) -> Result<()> {
    let baseline = read_bar_color_baseline(baseline_path)?;
    let mut drifted_bar_channel_names: Vec<String> = Vec::new();

    for ((bar_name, measured_color), baseline_color) in
        SMPTE_BAR_NAMES.iter().zip(measured).zip(&baseline)
    {
        for ((channel_name, measured_channel), (_, baseline_channel)) in measured_color
            .by_baseline_channel_name()
            .into_iter()
            .zip(baseline_color.by_baseline_channel_name())
        {
            let drift = (measured_channel - baseline_channel).abs();
            if drift <= tolerance {
                tracing::info!(
                    "    {bar_name}.{channel_name} drift: {drift:.4}  (limit {tolerance})  PASS"
                );
            } else {
                tracing::error!(
                    "    {bar_name}.{channel_name} drift: {drift:.4}  (limit {tolerance})  FAIL"
                );
                drifted_bar_channel_names.push(format!("{bar_name}.{channel_name}"));
            }
        }
    }
    tracing::info!("══════════════════════════════════════════════════════════════════");

    anyhow::ensure!(
        drifted_bar_channel_names.is_empty(),
        "bar colour drift outside ±{tolerance} on {} — a colour-management regression is \
         the first thing to look for (baseline: {})",
        drifted_bar_channel_names.join(", "),
        baseline_path.display()
    );
    tracing::info!("[test-pattern-color] RESULT: PASS");
    Ok(())
}

fn read_bar_color_baseline(baseline_path: &Path) -> Result<SmpteBarColors> {
    let baseline_text = std::fs::read_to_string(baseline_path)
        .with_context(|| format!("reading the baseline at {}", baseline_path.display()))?;

    let mut colors_by_bar_name: BTreeMap<&str, RgbOnTheUnitScale> = BTreeMap::new();
    for line in baseline_text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').map(str::trim).collect();
        let [bar_name, red, green, blue] = fields[..] else {
            continue;
        };
        if !SMPTE_BAR_NAMES.contains(&bar_name) {
            continue;
        }
        let parsed_channel = |channel_text: &str| -> Result<f64> {
            channel_text.parse::<f64>().with_context(|| {
                format!(
                    "the baseline at {} reads `{channel_text}` for bar `{bar_name}`",
                    baseline_path.display()
                )
            })
        };
        colors_by_bar_name.insert(
            bar_name,
            RgbOnTheUnitScale {
                red: parsed_channel(red)?,
                green: parsed_channel(green)?,
                blue: parsed_channel(blue)?,
            },
        );
    }

    let mut baseline_bar_colors = [RgbOnTheUnitScale::default(); SMPTE_BAR_COUNT];
    for (bar_name, baseline_bar_color) in SMPTE_BAR_NAMES.iter().zip(&mut baseline_bar_colors) {
        *baseline_bar_color = *colors_by_bar_name.get(bar_name).with_context(|| {
            format!(
                "the baseline at {} carries no `{bar_name}` row — capture it with \
                 `--capture-baseline`",
                baseline_path.display()
            )
        })?;
    }
    Ok(baseline_bar_colors)
}

fn write_bar_color_baseline(
    baseline_path: &Path,
    measured: &SmpteBarColors,
    baseline_notes: &[String],
) -> Result<()> {
    let mut baseline_tsv = String::from(
        "# Test-pattern colour round trip: each SMPTE bar's median colour.\n\
         # Generated by runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_test_pattern.sh\n\
         # Captured with: BASELINE_CAPTURE=1 e2e_fixture_psnr_test_pattern.sh <out> <codec>\n",
    );
    for baseline_note in baseline_notes {
        writeln!(baseline_tsv, "# {baseline_note}")?;
    }
    writeln!(
        baseline_tsv,
        "# Default verification tolerance is ±{DEFAULT_BAR_COLOR_DRIFT_TOLERANCE} absolute on \
         the [0,1] scale, per bar and channel.\nbar\tr\tg\tb"
    )?;
    for (bar_name, bar_color) in SMPTE_BAR_NAMES.iter().zip(measured) {
        writeln!(
            baseline_tsv,
            "{bar_name}\t{:.4}\t{:.4}\t{:.4}",
            bar_color.red, bar_color.green, bar_color.blue
        )?;
    }

    write_file_creating_parents(baseline_path, &baseline_tsv)?;
    tracing::info!("  Baseline written: {}", baseline_path.display());
    tracing::info!("[test-pattern-color] BASELINE CAPTURED");
    Ok(())
}

/// Every PNG directly under a directory, in sorted filename order so a run's
/// report reads the same way twice.
fn sorted_png_paths_in(directory: &Path) -> Result<Vec<PathBuf>> {
    let entries =
        std::fs::read_dir(directory).with_context(|| format!("reading {}", directory.display()))?;
    let mut png_paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
        })
        .collect();
    png_paths.sort();
    Ok(png_paths)
}

fn file_stem_of(path: &Path) -> Result<String> {
    Ok(path
        .file_stem()
        .with_context(|| format!("{} has no file name", path.display()))?
        .to_string_lossy()
        .into_owned())
}

fn write_file_creating_parents(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a solid-colour PNG, the smallest thing that exercises the whole
    /// read → pair → score path.
    fn write_solid_color_png(directory: &Path, file_name: &str, rgb: [u8; 3]) -> PathBuf {
        std::fs::create_dir_all(directory).unwrap();
        let png_path = directory.join(file_name);
        let file = std::fs::File::create(&png_path).unwrap();
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), 8, 8);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let rgba_bytes: Vec<u8> = std::iter::repeat_n([rgb[0], rgb[1], rgb[2], 0xFF], 8 * 8)
            .flatten()
            .collect();
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&rgba_bytes)
            .unwrap();
        png_path
    }

    /// A reference set and a decoded set that round-tripped it perfectly.
    fn scratch_run_with_one_perfect_sample_per_reference() -> tempfile::TempDir {
        let scratch = tempfile::tempdir().unwrap();
        for (reference_stem, rgb) in [("solid_red", [255, 0, 0]), ("solid_grey", [128, 128, 128])] {
            write_solid_color_png(
                &scratch.path().join("references"),
                &format!("{reference_stem}.png"),
                rgb,
            );
            write_solid_color_png(
                &scratch.path().join("decoded"),
                &format!("{reference_stem}__0.png"),
                rgb,
            );
        }
        scratch
    }

    fn score(scratch: &tempfile::TempDir, inject: Option<InjectedColorRegression>) -> Result<()> {
        score_decoded_frames_against_references(
            &scratch.path().join("decoded"),
            &scratch.path().join("references"),
            inject,
            Some(&scratch.path().join("psnr_report.tsv")),
        )
    }

    #[test]
    fn a_decoded_sample_scores_against_the_reference_its_name_states() {
        let scratch = scratch_run_with_one_perfect_sample_per_reference();
        score(&scratch, None).expect("a perfect round trip passes");

        let report = std::fs::read_to_string(scratch.path().join("psnr_report.tsv")).unwrap();
        assert_eq!(
            report,
            "reference\tdecoded_sample\ty_db\tu_db\tv_db\tverdict\n\
             solid_grey\tsolid_grey__0\tinf\tinf\tinf\tPASS\n\
             solid_red\tsolid_red__0\tinf\tinf\tinf\tPASS\n"
        );
    }

    #[test]
    fn a_decoded_sample_naming_no_reference_is_refused_rather_than_paired_with_a_neighbour() {
        let scratch = scratch_run_with_one_perfect_sample_per_reference();
        write_solid_color_png(
            &scratch.path().join("decoded"),
            "solid_puce__0.png",
            [200, 100, 100],
        );

        let refusal = score(&scratch, None).expect_err("an unpairable sample stops the run");
        assert!(
            refusal.to_string().contains("solid_puce"),
            "the refusal must name the reference the sample claimed: {refusal}"
        );
    }

    #[test]
    fn a_reference_no_sample_landed_on_fails_the_run_rather_than_going_unmentioned() {
        let scratch = scratch_run_with_one_perfect_sample_per_reference();
        std::fs::remove_file(scratch.path().join("decoded/solid_red__0.png")).unwrap();

        let failure = score(&scratch, None)
            .expect_err("a reference the run never sampled is missing evidence, not a pass");
        assert!(
            failure.to_string().contains("solid_red"),
            "the failure must name what went unsampled: {failure}"
        );
        let report = std::fs::read_to_string(scratch.path().join("psnr_report.tsv")).unwrap();
        assert!(
            report.contains("solid_red\t-\tn/a\tn/a\tn/a\tNO-SAMPLE\n"),
            "the report keeps the unsampled reference as a row: {report}"
        );
    }

    #[test]
    fn an_injected_run_fails_and_the_report_names_the_sample_that_dropped() {
        let scratch = scratch_run_with_one_perfect_sample_per_reference();
        let failure = score(
            &scratch,
            Some(InjectedColorRegression::SwapRedAndBlueChannels),
        )
        .expect_err("the gate is vacuous if an injected run passes");
        assert!(
            failure.to_string().contains("solid_red__0"),
            "the failure names the samples below the floor: {failure}"
        );

        let report = std::fs::read_to_string(scratch.path().join("psnr_report.tsv")).unwrap();
        assert!(report.contains("solid_red__0"), "{report}");
        assert!(
            report.contains("\tFAIL\n"),
            "the swapped red must land as FAIL: {report}"
        );
        assert!(
            report.contains("solid_grey__0\tinf\tinf\tinf\tPASS"),
            "a greyscale frame carries no chroma to swap: {report}"
        );
    }

    #[test]
    fn a_run_that_captured_nothing_fails_rather_than_passing_an_empty_set() {
        let scratch = tempfile::tempdir().unwrap();
        write_solid_color_png(
            &scratch.path().join("references"),
            "solid_red.png",
            [255, 0, 0],
        );
        std::fs::create_dir_all(scratch.path().join("decoded")).unwrap();

        let failure =
            score(&scratch, None).expect_err("zero decoded frames is a failed run, not a pass");
        assert!(
            failure.to_string().contains("captured nothing to score"),
            "{failure}"
        );
    }

    /// Colours a decoded bar set might measure: each bar its nominal colour.
    fn nominal_bar_colors() -> SmpteBarColors {
        crate::codec_proof_image_measurement::SMPTE_BAR_NOMINAL_RGB.map(|rgb| {
            let [red, green, blue] = rgb.map(|channel| f64::from(channel) / 255.0);
            RgbOnTheUnitScale { red, green, blue }
        })
    }

    #[test]
    fn bar_colors_pass_inside_the_tolerance_and_fail_outside_it_naming_the_bar() {
        let scratch = tempfile::tempdir().unwrap();
        let baseline_path = scratch.path().join("baseline.tsv");
        write_bar_color_baseline(&baseline_path, &nominal_bar_colors(), &[]).unwrap();

        let mut measured = nominal_bar_colors();
        measured[5].green = 0.02;
        compare_bar_colors_to_baseline(&baseline_path, &measured, 0.05)
            .expect("0.02 of drift is inside a 0.05 tolerance");

        measured[5].green = 0.0965;
        let failure = compare_bar_colors_to_baseline(&baseline_path, &measured, 0.05)
            .expect_err("the bt601/bt709 green rise on red is outside a 0.05 tolerance");
        assert!(
            failure.to_string().contains("red.g"),
            "the failure names the drifted bar and channel: {failure}"
        );
    }

    #[test]
    fn a_captured_baseline_reads_back_as_the_colours_that_wrote_it() {
        let scratch = tempfile::tempdir().unwrap();
        let baseline_path = scratch
            .path()
            .join("psnr_test_pattern_bar_colors_baseline.tsv");
        let mut captured = nominal_bar_colors();
        captured[5] = RgbOnTheUnitScale {
            red: 0.9180,
            green: 0.0575,
            blue: 0.0536,
        };
        write_bar_color_baseline(&baseline_path, &captured, &["Codec: h264".to_string()]).unwrap();

        let baseline_text = std::fs::read_to_string(&baseline_path).unwrap();
        assert!(
            baseline_text.contains("# Codec: h264"),
            "the provenance line is what makes the numbers reproducible: {baseline_text}"
        );
        // Round-tripping at the tightest tolerance the written precision allows
        // is what proves the writer and the reader agree on the format.
        compare_bar_colors_to_baseline(&baseline_path, &captured, 0.0001).unwrap();
    }

    #[test]
    fn a_baseline_missing_a_bar_is_refused_rather_than_read_as_black() {
        let scratch = tempfile::tempdir().unwrap();
        let baseline_path = scratch.path().join("baseline.tsv");
        write_bar_color_baseline(&baseline_path, &nominal_bar_colors(), &[]).unwrap();
        let without_blue: String = std::fs::read_to_string(&baseline_path)
            .unwrap()
            .lines()
            .filter(|line| !line.starts_with("blue\t"))
            .map(|line| format!("{line}\n"))
            .collect();
        std::fs::write(&baseline_path, without_blue).unwrap();

        let refusal = compare_bar_colors_to_baseline(&baseline_path, &nominal_bar_colors(), 0.05)
            .expect_err("a missing bar read as black would silently pass a blue regression");
        assert!(refusal.to_string().contains("no `blue` row"), "{refusal}");
    }

    #[test]
    fn only_png_files_are_read_out_of_a_run_directory() {
        let scratch = scratch_run_with_one_perfect_sample_per_reference();
        std::fs::write(scratch.path().join("decoded/pipeline.log"), "not an image").unwrap();
        score(&scratch, None).expect("a log file beside the frames is not a frame");
    }

    #[test]
    fn the_measured_set_is_read_in_sorted_order_so_a_report_reads_the_same_twice() {
        let scratch = tempfile::tempdir().unwrap();
        for file_name in ["c.png", "a.png", "b.png"] {
            write_solid_color_png(scratch.path(), file_name, [10, 20, 30]);
        }
        let sorted: Vec<String> = sorted_png_paths_in(scratch.path())
            .unwrap()
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(sorted, ["a.png", "b.png", "c.png"]);
    }
}
