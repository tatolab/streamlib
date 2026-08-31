// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `cargo xtask psnr` — the codec proof's scorer.
//!
//! Two verbs over one set of measurements ([`crate::codec_proof_image_measurement`]):
//! `score` compares a decoded frame set to the references that produced it,
//! and `channel-means` is the vivid colorimetry rig's drift lock. They share a
//! tool so the bug-injection modes are defined once — the modes are what make
//! either gate non-vacuous, and two definitions of a mode can disagree.
//!
//! Frames reach this tool as PNGs on disk, written by
//! `streamlib exchange --channel`: the rig is tapped for bags, each sampled
//! surface id is exchanged for that frame's exact pixels over the control
//! plane, and the run's own graph is never altered to be observed. Nothing here
//! touches a GPU, which is why its tests run in CI while the runs that produce
//! its input do not.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Subcommand;

use crate::codec_proof_image_measurement::{
    InjectedColorRegression, LUMA_PSNR_PASS_FLOOR_DB, LUMA_PSNR_WARN_FLOOR_DB,
    PlanePeakSignalToNoiseRatio, ReferenceComparisonVerdict, RgbChannelMeans, Rgba8Image,
};

/// What separates a decoded sample's reference stem from the rest of its file
/// name: `solid_red__0.png` scores against `solid_red.png`.
///
/// The fixture script names the files, and this is the whole pairing contract
/// between them. It is a name rather than a bag field because the decoded side
/// carries nothing to pair on — `sequence_index` is an encoded-frame field and
/// a decoded bag is an ordinary video frame — so the rig is driven once per
/// reference and the pairing is exact by construction.
const DECODED_SAMPLE_REFERENCE_STEM_SEPARATOR: &str = "__";

/// Default absolute drift a channel mean may carry from its baseline, on the
/// `[0, 1]` scale.
const DEFAULT_CHANNEL_MEAN_DRIFT_TOLERANCE: f64 = 0.05;

#[derive(Subcommand)]
pub enum PsnrCommand {
    /// Score decoded frames against the reference PNGs that produced them:
    /// per-plane Y/U/V PSNR, classified Y >= 35 dB pass / 30-35 warn / < 30
    /// fail. Exits non-zero when any reference fails or went unsampled.
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

    /// The vivid colorimetry drift lock: rig-wide mean of each RGB channel
    /// across a decoded frame set, compared to a checked-in baseline TSV.
    /// Vivid produces no ground truth to score PSNR against, so a saturated
    /// test pattern's channel means are what catches a matrix
    /// mis-interpretation there.
    ChannelMeans {
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
        /// Absolute drift a channel may carry, on the `[0, 1]` scale.
        #[arg(long, default_value_t = DEFAULT_CHANNEL_MEAN_DRIFT_TOLERANCE)]
        tolerance: f64,
        /// Overwrite the baseline with this run's means instead of comparing.
        #[arg(long)]
        capture_baseline: bool,
        /// Provenance line recorded in a captured baseline's header — the
        /// vivid test pattern the run forced, which is what makes the numbers
        /// reproducible.
        #[arg(long)]
        baseline_note: Option<String>,
        /// Write the per-frame means as TSV.
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
        PsnrCommand::ChannelMeans {
            images,
            baseline,
            inject,
            tolerance,
            capture_baseline,
            baseline_note,
            report,
        } => {
            let measured = measure_channel_means(&images, inject, report.as_deref())?;
            if capture_baseline {
                write_channel_mean_baseline(&baseline, measured, baseline_note.as_deref())
            } else {
                compare_channel_means_to_baseline(&baseline, measured, tolerance)
            }
        }
    }
}

/// One decoded sample scored against its reference.
struct ScoredReferenceComparison {
    reference_stem: String,
    decoded_file_name: String,
    luma_ratio: PlanePeakSignalToNoiseRatio,
    blue_difference_chroma_ratio: PlanePeakSignalToNoiseRatio,
    red_difference_chroma_ratio: PlanePeakSignalToNoiseRatio,
    verdict: ReferenceComparisonVerdict,
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

    let mut scored: Vec<ScoredReferenceComparison> = Vec::new();
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
        scored.push(score_one_pair(
            reference_stem,
            decoded_file_name,
            &decoded,
            reference,
        )?);
    }

    let sampled_reference_stems: std::collections::BTreeSet<&str> = scored
        .iter()
        .map(|comparison| comparison.reference_stem.as_str())
        .collect();
    let unsampled_reference_stems: Vec<&String> = references_by_stem
        .keys()
        .filter(|stem| !sampled_reference_stems.contains(stem.as_str()))
        .collect();

    report_scored_comparisons(&scored, &unsampled_reference_stems, report_path)?;

    let failed: Vec<&str> = scored
        .iter()
        .filter(|comparison| comparison.verdict == ReferenceComparisonVerdict::Fail)
        .map(|comparison| comparison.decoded_file_name.as_str())
        .collect();
    anyhow::ensure!(
        failed.is_empty() && unsampled_reference_stems.is_empty(),
        "PSNR gate FAILED — {} sample(s) below {LUMA_PSNR_WARN_FLOOR_DB} dB Y ({}), \
         {} reference(s) never sampled ({})",
        failed.len(),
        if failed.is_empty() {
            "none".to_string()
        } else {
            failed.join(", ")
        },
        unsampled_reference_stems.len(),
        if unsampled_reference_stems.is_empty() {
            "none".to_string()
        } else {
            unsampled_reference_stems
                .iter()
                .map(|stem| stem.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
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

    let decoded_planes = cropped.to_bt709_full_range_yuv420_planes();
    let reference_planes = reference.to_bt709_full_range_yuv420_planes();
    let luma_ratio = PlanePeakSignalToNoiseRatio::between(
        &decoded_planes.luma_plane,
        &reference_planes.luma_plane,
    )?;

    Ok(ScoredReferenceComparison {
        reference_stem,
        decoded_file_name,
        luma_ratio,
        blue_difference_chroma_ratio: PlanePeakSignalToNoiseRatio::between(
            &decoded_planes.blue_difference_chroma_plane,
            &reference_planes.blue_difference_chroma_plane,
        )?,
        red_difference_chroma_ratio: PlanePeakSignalToNoiseRatio::between(
            &decoded_planes.red_difference_chroma_plane,
            &reference_planes.red_difference_chroma_plane,
        )?,
        verdict: ReferenceComparisonVerdict::for_luma_ratio(luma_ratio),
    })
}

fn report_scored_comparisons(
    scored: &[ScoredReferenceComparison],
    unsampled_reference_stems: &[&String],
    report_path: Option<&Path>,
) -> Result<()> {
    tracing::info!("══════════════════════════════════════════════════════════════════");
    tracing::info!(
        "  Fixture PSNR (Y >= {LUMA_PSNR_PASS_FLOOR_DB} pass, \
         {LUMA_PSNR_WARN_FLOOR_DB}-{LUMA_PSNR_PASS_FLOOR_DB} warn, \
         < {LUMA_PSNR_WARN_FLOOR_DB} fail)"
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
    for comparison in scored {
        tracing::info!(
            "  {:<28}  {:>8}  {:>8}  {:>8}   {}",
            comparison.decoded_file_name,
            comparison.luma_ratio.as_report_column(),
            comparison.blue_difference_chroma_ratio.as_report_column(),
            comparison.red_difference_chroma_ratio.as_report_column(),
            comparison.verdict.as_report_column()
        );
        report_tsv.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            comparison.reference_stem,
            comparison.decoded_file_name,
            comparison.luma_ratio.as_report_column(),
            comparison.blue_difference_chroma_ratio.as_report_column(),
            comparison.red_difference_chroma_ratio.as_report_column(),
            comparison.verdict.as_report_column()
        ));
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
        report_tsv.push_str(&format!(
            "{unsampled_reference_stem}\t-\tn/a\tn/a\tn/a\tNO-SAMPLE\n"
        ));
    }
    tracing::info!("══════════════════════════════════════════════════════════════════");

    if let Some(report_path) = report_path {
        write_file_creating_parents(report_path, &report_tsv)?;
        tracing::info!("  Report TSV: {}", report_path.display());
    }
    Ok(())
}

fn measure_channel_means(
    images_directory: &Path,
    inject: Option<InjectedColorRegression>,
    report_path: Option<&Path>,
) -> Result<RgbChannelMeans> {
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

    let mut report_tsv = String::from("sample\tr_mean\tg_mean\tb_mean\n");
    let mut per_frame_means: Vec<RgbChannelMeans> = Vec::with_capacity(image_paths.len());
    for image_path in &image_paths {
        let mut image = Rgba8Image::read_png(image_path)?;
        if let Some(regression) = inject {
            image = image.with_injected_color_regression(regression);
        }
        let means = image.rgb_channel_means();
        report_tsv.push_str(&format!(
            "{}\t{:.6}\t{:.6}\t{:.6}\n",
            image_path.file_name().unwrap_or_default().to_string_lossy(),
            means.red,
            means.green,
            means.blue
        ));
        per_frame_means.push(means);
    }

    let rig_wide_means = RgbChannelMeans::averaged(&per_frame_means)
        .expect("the image list was checked non-empty above");
    tracing::info!("══════════════════════════════════════════════════════════════════");
    tracing::info!("  Vivid colour round-trip channel means");
    tracing::info!("══════════════════════════════════════════════════════════════════");
    tracing::info!("  Samples:  {}", per_frame_means.len());
    tracing::info!("  Mean R:   {:.4}", rig_wide_means.red);
    tracing::info!("  Mean G:   {:.4}", rig_wide_means.green);
    tracing::info!("  Mean B:   {:.4}", rig_wide_means.blue);

    if let Some(report_path) = report_path {
        write_file_creating_parents(report_path, &report_tsv)?;
        tracing::info!("  Per-sample stats: {}", report_path.display());
    }
    Ok(rig_wide_means)
}

fn compare_channel_means_to_baseline(
    baseline_path: &Path,
    measured: RgbChannelMeans,
    tolerance: f64,
) -> Result<()> {
    let baseline = read_channel_mean_baseline(baseline_path)?;
    let mut drifted_channel_names: Vec<&str> = Vec::new();

    for ((channel_name, measured_mean), (_, baseline_mean)) in measured
        .by_baseline_channel_name()
        .into_iter()
        .zip(baseline.by_baseline_channel_name())
    {
        let drift = (measured_mean - baseline_mean).abs();
        if drift <= tolerance {
            tracing::info!("    {channel_name} drift: {drift:.4}  (limit {tolerance})  PASS");
        } else {
            tracing::error!("    {channel_name} drift: {drift:.4}  (limit {tolerance})  FAIL");
            drifted_channel_names.push(channel_name);
        }
    }
    tracing::info!("══════════════════════════════════════════════════════════════════");

    anyhow::ensure!(
        drifted_channel_names.is_empty(),
        "channel-mean drift outside ±{tolerance} on {} — a colour-management regression is \
         the first thing to look for (baseline: {})",
        drifted_channel_names.join(", "),
        baseline_path.display()
    );
    tracing::info!("[vivid-color] RESULT: PASS");
    Ok(())
}

fn read_channel_mean_baseline(baseline_path: &Path) -> Result<RgbChannelMeans> {
    let baseline_text = std::fs::read_to_string(baseline_path)
        .with_context(|| format!("reading the baseline at {}", baseline_path.display()))?;

    let mut means_by_channel_name: BTreeMap<&str, f64> = BTreeMap::new();
    for line in baseline_text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((channel_name, mean)) = line.split_once('\t') else {
            continue;
        };
        if let Ok(mean) = mean.trim().parse::<f64>() {
            means_by_channel_name.insert(
                match channel_name.trim() {
                    "r" => "r",
                    "g" => "g",
                    "b" => "b",
                    _ => continue,
                },
                mean,
            );
        }
    }

    let channel_mean = |channel_name: &str| -> Result<f64> {
        means_by_channel_name
            .get(channel_name)
            .copied()
            .with_context(|| {
                format!(
                    "the baseline at {} carries no `{channel_name}` row — capture it with \
                     `--capture-baseline`",
                    baseline_path.display()
                )
            })
    };
    Ok(RgbChannelMeans {
        red: channel_mean("r")?,
        green: channel_mean("g")?,
        blue: channel_mean("b")?,
    })
}

fn write_channel_mean_baseline(
    baseline_path: &Path,
    measured: RgbChannelMeans,
    baseline_note: Option<&str>,
) -> Result<()> {
    let mut baseline_tsv = String::from(
        "# Vivid color-roundtrip channel-mean baseline.\n\
         # Generated by runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh\n\
         # Captured with: BASELINE_CAPTURE=1 e2e_fixture_psnr_vivid.sh <out> <codec>\n",
    );
    if let Some(baseline_note) = baseline_note {
        baseline_tsv.push_str(&format!("# {baseline_note}\n"));
    }
    baseline_tsv.push_str(&format!(
        "# Default verification tolerance is ±{DEFAULT_CHANNEL_MEAN_DRIFT_TOLERANCE} absolute on \
         the [0,1] scale.\nchannel\tmean\n"
    ));
    for (channel_name, mean) in measured.by_baseline_channel_name() {
        baseline_tsv.push_str(&format!("{channel_name}\t{mean:.4}\n"));
    }

    write_file_creating_parents(baseline_path, &baseline_tsv)?;
    tracing::info!("  Baseline written: {}", baseline_path.display());
    tracing::info!("[vivid-color] BASELINE CAPTURED");
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

    #[test]
    fn channel_means_pass_inside_the_tolerance_and_fail_outside_it() {
        let scratch = tempfile::tempdir().unwrap();
        let baseline_path = scratch.path().join("baseline.tsv");
        std::fs::write(
            &baseline_path,
            "channel\tmean\nr\t0.9000\ng\t0.0500\nb\t0.0500\n",
        )
        .unwrap();

        let measured = RgbChannelMeans {
            red: 0.92,
            green: 0.06,
            blue: 0.06,
        };
        compare_channel_means_to_baseline(&baseline_path, measured, 0.05)
            .expect("0.02 of drift is inside a 0.05 tolerance");

        let drifted = RgbChannelMeans {
            red: 0.80,
            green: 0.06,
            blue: 0.06,
        };
        let failure = compare_channel_means_to_baseline(&baseline_path, drifted, 0.05)
            .expect_err("0.10 of drift is outside a 0.05 tolerance");
        assert!(
            failure.to_string().contains(" r"),
            "the failure names the drifted channel: {failure}"
        );
    }

    #[test]
    fn a_captured_baseline_reads_back_as_the_means_that_wrote_it() {
        let scratch = tempfile::tempdir().unwrap();
        let baseline_path = scratch.path().join("psnr_vivid_baseline.tsv");
        let captured = RgbChannelMeans {
            red: 0.9180,
            green: 0.0575,
            blue: 0.0536,
        };
        write_channel_mean_baseline(&baseline_path, captured, Some("Vivid test_pattern: 7"))
            .unwrap();

        let baseline_text = std::fs::read_to_string(&baseline_path).unwrap();
        assert!(
            baseline_text.contains("# Vivid test_pattern: 7"),
            "the provenance line is what makes the numbers reproducible: {baseline_text}"
        );
        // Round-tripping at the tightest tolerance the written precision allows
        // is what proves the writer and the reader agree on the format.
        compare_channel_means_to_baseline(&baseline_path, captured, 0.0001).unwrap();
    }

    #[test]
    fn a_baseline_missing_a_channel_is_refused_rather_than_read_as_zero() {
        let scratch = tempfile::tempdir().unwrap();
        let baseline_path = scratch.path().join("baseline.tsv");
        std::fs::write(&baseline_path, "channel\tmean\nr\t0.9000\ng\t0.0500\n").unwrap();

        let refusal = compare_channel_means_to_baseline(
            &baseline_path,
            RgbChannelMeans {
                red: 0.9,
                green: 0.05,
                blue: 0.05,
            },
            0.05,
        )
        .expect_err("a missing channel read as 0.0 would silently pass a blue-channel regression");
        assert!(refusal.to_string().contains("no `b` row"), "{refusal}");
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
