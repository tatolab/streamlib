// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-frame accumulation and artifact emission for one measurement cell.
//!
//! Two accounting rules are easy to get wrong and are load-bearing for the
//! comparison:
//!
//! - The JSONL is raw. Every frame handed to the recorder appears in it,
//!   including the warmup-excluded ones and the clock-anomaly ones. The
//!   histograms, the drop count and the frame-rate windows see only the
//!   post-warmup frames. Re-deriving percentiles from the JSONL therefore
//!   requires re-applying the warmup filter — the two artifacts are not
//!   expected to agree by row count.
//! - Drops come from gaps in [`PerFrameLatencyMeasurement::frame_sequence_number`],
//!   never from timing. A slow frame is a latency outlier, not a drop.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::time::Duration;

use hdrhistogram::Histogram;
use hdrhistogram::serialization::V2DeflateSerializer;
use hdrhistogram::serialization::interval_log::{IntervalLogWriterBuilder, Tag};

/// 100ns is one order below `CLOCK_MONOTONIC`'s reported 1ns resolution and 60s
/// is two orders above the worst plausible stall, so no real sample lands
/// outside the range and every cell can share one bucket layout.
const HISTOGRAM_LOWEST_DISCERNIBLE_NANOSECONDS: u64 = 100;
const HISTOGRAM_HIGHEST_TRACKABLE_NANOSECONDS: u64 = 60_000_000_000;
const HISTOGRAM_SIGNIFICANT_FIGURES: u8 = 3;

const ONE_SECOND_NANOSECONDS: i64 = 1_000_000_000;

/// Interval-log tag for the Tier A headline quantity.
const SOURCE_EMIT_TO_SINK_RECEIVE_HISTOGRAM_TAG: &str = "source_emit_to_sink_receive";
/// Interval-log tag for time spent inside the stage callback.
const STAGE_CALLBACK_HISTOGRAM_TAG: &str = "stage_callback";

/// One frame's journey through the graph. All stamps are raw CLOCK_MONOTONIC nanoseconds.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct PerFrameLatencyMeasurement {
    pub frame_sequence_number: u64,
    pub source_emit_monotonic_nanoseconds: i64,
    pub sink_receive_monotonic_nanoseconds: i64,
    /// Time inside the stage callback itself (0 for the floor arm's no-op).
    pub stage_callback_nanoseconds: i64,
}

/// Percentiles for one measured quantity. Never carries a headline mean.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct LatencyPercentileSummary {
    pub sample_count: u64,
    pub p50_nanoseconds: u64,
    pub p99_nanoseconds: u64,
    pub p99_9_nanoseconds: u64,
    pub max_nanoseconds: u64,
}

impl LatencyPercentileSummary {
    /// The summary reported when a quantity has no post-warmup samples at all.
    fn empty() -> Self {
        Self {
            sample_count: 0,
            p50_nanoseconds: 0,
            p99_nanoseconds: 0,
            p99_9_nanoseconds: 0,
            max_nanoseconds: 0,
        }
    }
}

/// Accumulates one measurement cell's frames and writes its artifacts.
pub struct LatencyMeasurementRecorder {
    warmup_exclusion_nanoseconds: i64,
    first_source_emit_monotonic_nanoseconds: Option<i64>,
    every_received_frame_measurement: Vec<PerFrameLatencyMeasurement>,
    source_emit_to_sink_receive_histogram: Histogram<u64>,
    stage_callback_histogram: Histogram<u64>,
    measured_sink_receive_monotonic_nanoseconds: Vec<i64>,
    lowest_measured_frame_sequence_number: Option<u64>,
    highest_measured_frame_sequence_number: Option<u64>,
    measured_frame_count: u64,
    negative_latency_anomaly_count: u64,
    histogram_range_saturation_count: u64,
}

impl LatencyMeasurementRecorder {
    /// `warmup_exclusion_nanoseconds` drops early frames from the percentiles
    /// (the protocol excludes the first 60s of every cell).
    pub fn new(warmup_exclusion_nanoseconds: i64) -> Self {
        Self {
            warmup_exclusion_nanoseconds,
            first_source_emit_monotonic_nanoseconds: None,
            every_received_frame_measurement: Vec::new(),
            source_emit_to_sink_receive_histogram: new_nanosecond_histogram(),
            stage_callback_histogram: new_nanosecond_histogram(),
            measured_sink_receive_monotonic_nanoseconds: Vec::new(),
            lowest_measured_frame_sequence_number: None,
            highest_measured_frame_sequence_number: None,
            measured_frame_count: 0,
            negative_latency_anomaly_count: 0,
            histogram_range_saturation_count: 0,
        }
    }

    /// Take one frame's stamps, called once per frame the sink observes.
    pub fn record_frame_measurement(&mut self, measurement: PerFrameLatencyMeasurement) {
        self.every_received_frame_measurement.push(measurement);
        let first_source_emit_monotonic_nanoseconds = *self
            .first_source_emit_monotonic_nanoseconds
            .get_or_insert(measurement.source_emit_monotonic_nanoseconds);

        // Boundary rule: a frame whose emit stamp sits exactly on the warmup
        // deadline is MEASURED. The exclusion is "the first N nanoseconds", a
        // half-open interval, so the deadline itself is the first measured instant.
        let elapsed_since_first_emit_nanoseconds =
            measurement.source_emit_monotonic_nanoseconds - first_source_emit_monotonic_nanoseconds;
        if elapsed_since_first_emit_nanoseconds < self.warmup_exclusion_nanoseconds {
            return;
        }

        self.measured_frame_count += 1;
        self.measured_sink_receive_monotonic_nanoseconds
            .push(measurement.sink_receive_monotonic_nanoseconds);
        self.lowest_measured_frame_sequence_number = Some(
            self.lowest_measured_frame_sequence_number
                .map_or(measurement.frame_sequence_number, |lowest| {
                    lowest.min(measurement.frame_sequence_number)
                }),
        );
        self.highest_measured_frame_sequence_number = Some(
            self.highest_measured_frame_sequence_number
                .map_or(measurement.frame_sequence_number, |highest| {
                    highest.max(measurement.frame_sequence_number)
                }),
        );

        let source_emit_to_sink_receive_nanoseconds = measurement
            .sink_receive_monotonic_nanoseconds
            - measurement.source_emit_monotonic_nanoseconds;
        if source_emit_to_sink_receive_nanoseconds < 0 {
            // Never clamp to 0: a sink stamp preceding its own emit stamp means
            // the two arms are not on the same clock, which is the exact failure
            // the protocol's clock handshake exists to catch. Clamping would
            // publish it as a suspiciously fast p50.
            self.negative_latency_anomaly_count += 1;
        } else {
            record_nanoseconds_into_histogram(
                &mut self.source_emit_to_sink_receive_histogram,
                &mut self.histogram_range_saturation_count,
                source_emit_to_sink_receive_nanoseconds as u64,
            );
        }

        if measurement.stage_callback_nanoseconds < 0 {
            self.negative_latency_anomaly_count += 1;
        } else {
            record_nanoseconds_into_histogram(
                &mut self.stage_callback_histogram,
                &mut self.histogram_range_saturation_count,
                measurement.stage_callback_nanoseconds as u64,
            );
        }
    }

    /// Percentiles of the Tier A headline quantity over the measured frames.
    pub fn source_emit_to_sink_receive_summary(&self) -> LatencyPercentileSummary {
        summarize_nanosecond_histogram(&self.source_emit_to_sink_receive_histogram)
    }

    /// Percentiles of time spent inside the stage callback over the measured frames.
    pub fn stage_callback_summary(&self) -> LatencyPercentileSummary {
        summarize_nanosecond_histogram(&self.stage_callback_histogram)
    }

    /// Frames missing from the sequence — reported, not gated (owner decision).
    ///
    /// Computed as the measured sequence span minus the measured arrivals, so a
    /// frame that arrives out of order closes its own gap when it lands instead
    /// of being counted as a drop and then double-counted on arrival. Duplicate
    /// sequence numbers would under-count; an `every_sample` link does not
    /// produce them.
    pub fn dropped_frame_count(&self) -> u64 {
        match (
            self.lowest_measured_frame_sequence_number,
            self.highest_measured_frame_sequence_number,
        ) {
            (Some(lowest), Some(highest)) => {
                (highest - lowest + 1).saturating_sub(self.measured_frame_count)
            }
            _ => 0,
        }
    }

    /// Every frame handed to the recorder, warmup-excluded ones included. Read
    /// this before any percentile: a summary from an empty histogram is all
    /// zeroes, which reads like a very fast run rather than a refused one.
    pub fn received_frame_count(&self) -> u64 {
        self.every_received_frame_measurement.len() as u64
    }

    /// Post-warmup frames that entered the histograms and the drop accounting.
    pub fn measured_frame_count(&self) -> u64 {
        self.measured_frame_count
    }

    /// Samples whose computed duration was negative — a nonzero value here
    /// invalidates the cell's latency numbers rather than degrading them.
    pub fn negative_latency_anomaly_count(&self) -> u64 {
        self.negative_latency_anomaly_count
    }

    /// Samples above the histogram's 60s ceiling, recorded at the ceiling. A
    /// nonzero value means `max` and the top percentiles are floors, not values.
    pub fn histogram_range_saturation_count(&self) -> u64 {
        self.histogram_range_saturation_count
    }

    /// Rolling 1-second achieved-frame-rate windows, for the fps-stability check.
    ///
    /// One entry per measured frame whose full 1-second lookback fits inside the
    /// cell: the count of measured frames received in `(t - 1s, t]`, which at a
    /// steady 60fps is exactly 60.0. Sink-receive stamps are used because
    /// achieved frame rate is a property of the consumer, and they are
    /// non-decreasing because one single-threaded sink stamps them all.
    pub fn rolling_one_second_frame_rate_windows(&self) -> Vec<f64> {
        let sink_receive_stamps = &self.measured_sink_receive_monotonic_nanoseconds;
        let mut frame_rate_windows = Vec::new();
        let Some(&first_sink_receive_stamp) = sink_receive_stamps.first() else {
            return frame_rate_windows;
        };
        let mut window_start_index = 0usize;
        for (index, &sink_receive_stamp) in sink_receive_stamps.iter().enumerate() {
            let window_lower_bound_nanoseconds = sink_receive_stamp - ONE_SECOND_NANOSECONDS;
            while sink_receive_stamps[window_start_index] <= window_lower_bound_nanoseconds {
                window_start_index += 1;
            }
            if sink_receive_stamp - first_sink_receive_stamp < ONE_SECOND_NANOSECONDS {
                continue;
            }
            frame_rate_windows.push((index + 1 - window_start_index) as f64);
        }
        frame_rate_windows
    }

    pub fn write_per_frame_measurement_jsonl(&self, path: &Path) -> io::Result<()> {
        let mut buffered_jsonl_writer = BufWriter::new(File::create(path)?);
        for measurement in &self.every_received_frame_measurement {
            serde_json::to_writer(&mut buffered_jsonl_writer, measurement)?;
            buffered_jsonl_writer.write_all(b"\n")?;
        }
        buffered_jsonl_writer.flush()
    }

    /// Write both histograms as an HdrHistogram interval log (`.hlog`).
    ///
    /// Chosen for mergeability: it is the only HdrHistogram interchange format
    /// that carries several tagged histograms in one file, its V2+DEFLATE
    /// payloads decode back to full `Histogram` values, and because every cell
    /// shares the bucket layout fixed above, `Histogram::add` merges cells
    /// losslessly. Percentiles stored per cell could not be merged at all.
    pub fn write_mergeable_histogram_export(&self, path: &Path) -> io::Result<()> {
        let mut buffered_interval_log_writer = BufWriter::new(File::create(path)?);
        let mut histogram_serializer = V2DeflateSerializer::new();
        {
            // No `StartTime` header: it is wall-clock seconds-since-epoch, and
            // this spike stamps nothing but CLOCK_MONOTONIC. Intervals are
            // expressed as an offset from the cell's own start instead.
            let mut interval_log_writer = IntervalLogWriterBuilder::new()
                .add_comment("streamlib-pyembed-spike measurement cell; values are nanoseconds")
                .begin_log_with(&mut buffered_interval_log_writer, &mut histogram_serializer)?;
            let measured_interval_duration = self.measured_interval_duration();
            for (histogram_tag, histogram) in [
                (
                    SOURCE_EMIT_TO_SINK_RECEIVE_HISTOGRAM_TAG,
                    &self.source_emit_to_sink_receive_histogram,
                ),
                (STAGE_CALLBACK_HISTOGRAM_TAG, &self.stage_callback_histogram),
            ] {
                interval_log_writer
                    .write_histogram(
                        histogram,
                        Duration::ZERO,
                        measured_interval_duration,
                        Tag::new(histogram_tag),
                    )
                    .map_err(|error| {
                        io::Error::other(format!(
                            "interval log write failed for {histogram_tag}: {error:?}"
                        ))
                    })?;
            }
        }
        buffered_interval_log_writer.flush()
    }

    /// Span from the first to the last measured sink-receive stamp.
    fn measured_interval_duration(&self) -> Duration {
        let stamps = &self.measured_sink_receive_monotonic_nanoseconds;
        match (stamps.first(), stamps.last()) {
            (Some(&first), Some(&last)) if last > first => {
                Duration::from_nanos((last - first) as u64)
            }
            _ => Duration::ZERO,
        }
    }
}

fn new_nanosecond_histogram() -> Histogram<u64> {
    Histogram::<u64>::new_with_bounds(
        HISTOGRAM_LOWEST_DISCERNIBLE_NANOSECONDS,
        HISTOGRAM_HIGHEST_TRACKABLE_NANOSECONDS,
        HISTOGRAM_SIGNIFICANT_FIGURES,
    )
    .expect("the histogram bounds are compile-time constants and satisfy hdrhistogram's rules")
}

fn record_nanoseconds_into_histogram(
    histogram: &mut Histogram<u64>,
    histogram_range_saturation_count: &mut u64,
    value_nanoseconds: u64,
) {
    let recordable_nanoseconds = value_nanoseconds.min(HISTOGRAM_HIGHEST_TRACKABLE_NANOSECONDS);
    let record_failed = histogram.record(recordable_nanoseconds).is_err();
    if record_failed || recordable_nanoseconds != value_nanoseconds {
        *histogram_range_saturation_count += 1;
    }
}

fn summarize_nanosecond_histogram(histogram: &Histogram<u64>) -> LatencyPercentileSummary {
    if histogram.is_empty() {
        return LatencyPercentileSummary::empty();
    }
    LatencyPercentileSummary {
        sample_count: histogram.len(),
        p50_nanoseconds: histogram.value_at_quantile(0.5),
        p99_nanoseconds: histogram.value_at_quantile(0.99),
        p99_9_nanoseconds: histogram.value_at_quantile(0.999),
        max_nanoseconds: histogram.max(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hdrhistogram::serialization::interval_log::LogEntry;

    const WARMUP_EXCLUSION_NANOSECONDS: i64 = 60 * ONE_SECOND_NANOSECONDS;

    fn build_per_frame_latency_measurement(
        frame_sequence_number: u64,
        source_emit_monotonic_nanoseconds: i64,
        source_emit_to_sink_receive_nanoseconds: i64,
        stage_callback_nanoseconds: i64,
    ) -> PerFrameLatencyMeasurement {
        PerFrameLatencyMeasurement {
            frame_sequence_number,
            source_emit_monotonic_nanoseconds,
            sink_receive_monotonic_nanoseconds: source_emit_monotonic_nanoseconds
                + source_emit_to_sink_receive_nanoseconds,
            stage_callback_nanoseconds,
        }
    }

    fn temporary_artifact_path(file_name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "streamlib-pyembed-spike-{}-{file_name}",
            std::process::id()
        ))
    }

    /// The warmup boundary is half-open: a frame landing exactly on the deadline
    /// is measured. Flipping this to exclusive would silently shift every cell's
    /// sample set by one frame and make cells built by different code paths
    /// non-comparable.
    #[test]
    fn frame_exactly_on_the_warmup_deadline_is_measured() {
        let mut recorder = LatencyMeasurementRecorder::new(WARMUP_EXCLUSION_NANOSECONDS);
        recorder.record_frame_measurement(build_per_frame_latency_measurement(0, 1_000, 5_000, 0));
        recorder.record_frame_measurement(build_per_frame_latency_measurement(
            1,
            1_000 + WARMUP_EXCLUSION_NANOSECONDS - 1,
            5_000,
            0,
        ));
        recorder.record_frame_measurement(build_per_frame_latency_measurement(
            2,
            1_000 + WARMUP_EXCLUSION_NANOSECONDS,
            5_000,
            0,
        ));
        assert_eq!(recorder.received_frame_count(), 3);
        assert_eq!(recorder.measured_frame_count(), 1);
        assert_eq!(
            recorder.source_emit_to_sink_receive_summary().sample_count,
            1
        );
    }

    /// Warmup-excluded frames stay in the raw JSONL but must not reach the
    /// histograms or the drop count — otherwise the artifact and the summary
    /// disagree about which frames the cell measured.
    #[test]
    fn warmup_excluded_frames_reach_the_jsonl_but_not_the_histograms() {
        let mut recorder = LatencyMeasurementRecorder::new(WARMUP_EXCLUSION_NANOSECONDS);
        for frame_sequence_number in 0..10u64 {
            recorder.record_frame_measurement(build_per_frame_latency_measurement(
                frame_sequence_number,
                frame_sequence_number as i64 * 1_000_000,
                5_000,
                0,
            ));
        }
        assert_eq!(recorder.received_frame_count(), 10);
        assert_eq!(recorder.measured_frame_count(), 0);
        assert_eq!(recorder.dropped_frame_count(), 0);
        assert_eq!(
            recorder.source_emit_to_sink_receive_summary().sample_count,
            0
        );

        let jsonl_path = temporary_artifact_path("warmup-excluded.jsonl");
        recorder
            .write_per_frame_measurement_jsonl(&jsonl_path)
            .expect("jsonl write");
        let jsonl_contents = std::fs::read_to_string(&jsonl_path).expect("jsonl read");
        assert_eq!(jsonl_contents.lines().count(), 10);
        std::fs::remove_file(&jsonl_path).ok();
    }

    /// Drops are gaps in the sequence, never timing. A missing sequence number
    /// must be counted exactly once.
    #[test]
    fn dropped_frame_count_counts_a_synthetic_sequence_gap() {
        let mut recorder = LatencyMeasurementRecorder::new(0);
        for frame_sequence_number in [0u64, 1, 2, 5, 6] {
            recorder.record_frame_measurement(build_per_frame_latency_measurement(
                frame_sequence_number,
                frame_sequence_number as i64 * 16_666_667,
                5_000,
                0,
            ));
        }
        assert_eq!(recorder.received_frame_count(), 5);
        assert_eq!(recorder.dropped_frame_count(), 2);
    }

    /// The very first observed frame has no predecessor, so it must not be
    /// charged a gap against sequence number 0 or against nothing at all.
    #[test]
    fn first_observed_frame_reports_no_drop() {
        let mut recorder = LatencyMeasurementRecorder::new(0);
        recorder.record_frame_measurement(build_per_frame_latency_measurement(7, 1_000, 5_000, 0));
        assert_eq!(recorder.dropped_frame_count(), 0);
        assert_eq!(recorder.received_frame_count(), 1);
    }

    /// A frame arriving after its successor must close its own gap rather than
    /// leave a permanent phantom drop in the report.
    #[test]
    fn out_of_order_arrival_reports_no_phantom_drop() {
        let mut recorder = LatencyMeasurementRecorder::new(0);
        for frame_sequence_number in [0u64, 2, 1, 3] {
            recorder.record_frame_measurement(build_per_frame_latency_measurement(
                frame_sequence_number,
                frame_sequence_number as i64 * 16_666_667,
                5_000,
                0,
            ));
        }
        assert_eq!(recorder.dropped_frame_count(), 0);
    }

    /// A run where the sink received nothing must report zero samples, not
    /// panic and not divide by zero — a refused run is the most likely failure
    /// mode of the embedded arm and must be legible as such.
    #[test]
    fn no_frames_received_reports_a_zero_count_summary() {
        let recorder = LatencyMeasurementRecorder::new(WARMUP_EXCLUSION_NANOSECONDS);
        assert_eq!(recorder.received_frame_count(), 0);
        assert_eq!(recorder.dropped_frame_count(), 0);
        assert!(recorder.rolling_one_second_frame_rate_windows().is_empty());

        let summary = recorder.source_emit_to_sink_receive_summary();
        assert_eq!(summary.sample_count, 0);
        assert_eq!(summary.p50_nanoseconds, 0);
        assert_eq!(summary.p99_nanoseconds, 0);
        assert_eq!(summary.p99_9_nanoseconds, 0);
        assert_eq!(summary.max_nanoseconds, 0);
        assert_eq!(recorder.stage_callback_summary().sample_count, 0);
    }

    /// A sink stamp preceding its emit stamp means the arms disagree on the
    /// clock. Clamping it to 0 would publish the disagreement as a fast p50, so
    /// it must be counted and kept out of the histogram.
    #[test]
    fn negative_latency_increments_the_anomaly_counter_and_skips_the_histogram() {
        let mut recorder = LatencyMeasurementRecorder::new(0);
        recorder.record_frame_measurement(build_per_frame_latency_measurement(
            0, 1_000_000, 2_000_000, 0,
        ));
        recorder.record_frame_measurement(build_per_frame_latency_measurement(
            1, 2_000_000, -500_000, 0,
        ));
        assert_eq!(recorder.negative_latency_anomaly_count(), 1);
        assert_eq!(recorder.received_frame_count(), 2);
        assert_eq!(recorder.measured_frame_count(), 2);
        assert_eq!(
            recorder.source_emit_to_sink_receive_summary().sample_count,
            1
        );
    }

    /// A negative stage-callback duration is single-clock and single-process, so
    /// it means instrumentation is broken; it must surface, not be absorbed.
    #[test]
    fn negative_stage_callback_duration_increments_the_anomaly_counter() {
        let mut recorder = LatencyMeasurementRecorder::new(0);
        recorder.record_frame_measurement(build_per_frame_latency_measurement(
            0, 1_000_000, 500_000, -1,
        ));
        assert_eq!(recorder.negative_latency_anomaly_count(), 1);
        assert_eq!(recorder.stage_callback_summary().sample_count, 0);
        assert_eq!(
            recorder.source_emit_to_sink_receive_summary().sample_count,
            1
        );
    }

    /// Hand-computed distribution: 990 samples at 1ms and 10 at 100ms. p50 and
    /// p99 must both land on 1ms (the p99 cut falls exactly on the 990th
    /// sample), p99.9 and max on 100ms. Guards an off-by-one in the quantile
    /// wiring, which would move the headline number by two orders of magnitude.
    #[test]
    fn percentiles_match_a_hand_computed_distribution() {
        const ONE_MILLISECOND_NANOSECONDS: i64 = 1_000_000;
        const ONE_HUNDRED_MILLISECONDS_NANOSECONDS: i64 = 100_000_000;
        let mut recorder = LatencyMeasurementRecorder::new(0);
        for frame_sequence_number in 0..1_000u64 {
            let latency_nanoseconds = if frame_sequence_number < 990 {
                ONE_MILLISECOND_NANOSECONDS
            } else {
                ONE_HUNDRED_MILLISECONDS_NANOSECONDS
            };
            recorder.record_frame_measurement(build_per_frame_latency_measurement(
                frame_sequence_number,
                frame_sequence_number as i64 * 16_666_667,
                latency_nanoseconds,
                0,
            ));
        }

        let summary = recorder.source_emit_to_sink_receive_summary();
        assert_eq!(summary.sample_count, 1_000);
        assert_within_three_significant_figures(
            summary.p50_nanoseconds,
            ONE_MILLISECOND_NANOSECONDS,
        );
        assert_within_three_significant_figures(
            summary.p99_nanoseconds,
            ONE_MILLISECOND_NANOSECONDS,
        );
        assert_within_three_significant_figures(
            summary.p99_9_nanoseconds,
            ONE_HUNDRED_MILLISECONDS_NANOSECONDS,
        );
        assert_within_three_significant_figures(
            summary.max_nanoseconds,
            ONE_HUNDRED_MILLISECONDS_NANOSECONDS,
        );
    }

    fn assert_within_three_significant_figures(actual_nanoseconds: u64, expected_nanoseconds: i64) {
        let expected = expected_nanoseconds as f64;
        let relative_error = (actual_nanoseconds as f64 - expected).abs() / expected;
        assert!(
            relative_error < 0.002,
            "{actual_nanoseconds}ns is not within 3 significant figures of {expected_nanoseconds}ns"
        );
    }

    /// A perfectly paced stream must report exactly its nominal rate in every
    /// rolling window; an off-by-one in the half-open window bounds would show
    /// as a permanent 49 or 51 and be misread as pacing jitter. 50fps is used
    /// rather than 60 because its period divides one second exactly, so the
    /// expected count is arithmetic rather than a rounding artifact.
    #[test]
    fn rolling_windows_report_the_nominal_rate_of_a_perfectly_paced_stream() {
        const FIFTY_FPS_PERIOD_NANOSECONDS: i64 = ONE_SECOND_NANOSECONDS / 50;
        let mut recorder = LatencyMeasurementRecorder::new(0);
        for frame_sequence_number in 0..300u64 {
            recorder.record_frame_measurement(build_per_frame_latency_measurement(
                frame_sequence_number,
                frame_sequence_number as i64 * FIFTY_FPS_PERIOD_NANOSECONDS,
                500_000,
                0,
            ));
        }
        let frame_rate_windows = recorder.rolling_one_second_frame_rate_windows();
        assert_eq!(frame_rate_windows.len(), 300 - 50);
        assert!(
            frame_rate_windows.iter().all(|&rate| rate == 50.0),
            "expected every rolling window to report 50fps, got {frame_rate_windows:?}"
        );
    }

    /// The JSONL is the raw record every later analysis re-derives from: one
    /// compact object per line, no wrapping array, every field intact.
    #[test]
    fn jsonl_round_trips_line_by_line_through_serde_json() {
        let mut recorder = LatencyMeasurementRecorder::new(0);
        let written_measurements: Vec<PerFrameLatencyMeasurement> = (0..5u64)
            .map(|frame_sequence_number| {
                build_per_frame_latency_measurement(
                    frame_sequence_number,
                    frame_sequence_number as i64 * 16_666_667,
                    1_234_567,
                    89_012,
                )
            })
            .collect();
        for measurement in &written_measurements {
            recorder.record_frame_measurement(*measurement);
        }

        let jsonl_path = temporary_artifact_path("round-trip.jsonl");
        recorder
            .write_per_frame_measurement_jsonl(&jsonl_path)
            .expect("jsonl write");
        let jsonl_contents = std::fs::read_to_string(&jsonl_path).expect("jsonl read");
        std::fs::remove_file(&jsonl_path).ok();

        assert!(!jsonl_contents.starts_with('['));
        let parsed_lines: Vec<serde_json::Value> = jsonl_contents
            .lines()
            .map(|line| serde_json::from_str(line).expect("each line is a standalone JSON object"))
            .collect();
        assert_eq!(parsed_lines.len(), written_measurements.len());
        for (parsed, written) in parsed_lines.iter().zip(&written_measurements) {
            assert_eq!(
                parsed["frame_sequence_number"],
                written.frame_sequence_number
            );
            assert_eq!(
                parsed["source_emit_monotonic_nanoseconds"],
                written.source_emit_monotonic_nanoseconds
            );
            assert_eq!(
                parsed["sink_receive_monotonic_nanoseconds"],
                written.sink_receive_monotonic_nanoseconds
            );
            assert_eq!(
                parsed["stage_callback_nanoseconds"],
                written.stage_callback_nanoseconds
            );
        }
    }

    /// The histogram export has to decode back into mergeable histograms — if it
    /// did not, per-cell files could not be combined and the protocol's
    /// cross-cell percentiles would be uncomputable.
    #[test]
    fn histogram_export_decodes_and_merges_across_cells() {
        let mut first_cell_recorder = LatencyMeasurementRecorder::new(0);
        let mut second_cell_recorder = LatencyMeasurementRecorder::new(0);
        for frame_sequence_number in 0..100u64 {
            first_cell_recorder.record_frame_measurement(build_per_frame_latency_measurement(
                frame_sequence_number,
                frame_sequence_number as i64 * 16_666_667,
                2_000_000,
                0,
            ));
            second_cell_recorder.record_frame_measurement(build_per_frame_latency_measurement(
                frame_sequence_number,
                frame_sequence_number as i64 * 16_666_667,
                4_000_000,
                0,
            ));
        }

        let first_cell_path = temporary_artifact_path("first-cell.hlog");
        let second_cell_path = temporary_artifact_path("second-cell.hlog");
        first_cell_recorder
            .write_mergeable_histogram_export(&first_cell_path)
            .expect("first cell export");
        second_cell_recorder
            .write_mergeable_histogram_export(&second_cell_path)
            .expect("second cell export");

        let mut merged_histogram = new_nanosecond_histogram();
        let mut merged_sample_count = 0u64;
        for path in [&first_cell_path, &second_cell_path] {
            let log_bytes = std::fs::read(path).expect("hlog read");
            let mut histogram_deserializer = hdrhistogram::serialization::Deserializer::new();
            for log_entry in
                hdrhistogram::serialization::interval_log::IntervalLogIterator::new(&log_bytes)
            {
                let LogEntry::Interval(interval_log_histogram) =
                    log_entry.expect("hlog entry parses")
                else {
                    continue;
                };
                if interval_log_histogram.tag().map(|tag| tag.as_str())
                    != Some(SOURCE_EMIT_TO_SINK_RECEIVE_HISTOGRAM_TAG)
                {
                    continue;
                }
                let encoded_histogram_bytes =
                    base64_decode_histogram(interval_log_histogram.encoded_histogram());
                let decoded_histogram: Histogram<u64> = histogram_deserializer
                    .deserialize(&mut encoded_histogram_bytes.as_slice())
                    .expect("histogram deserializes");
                merged_sample_count += decoded_histogram.len();
                merged_histogram
                    .add(&decoded_histogram)
                    .expect("cells share bounds");
            }
        }
        std::fs::remove_file(&first_cell_path).ok();
        std::fs::remove_file(&second_cell_path).ok();

        assert_eq!(merged_sample_count, 200);
        assert_eq!(merged_histogram.len(), 200);
        assert_within_three_significant_figures(merged_histogram.value_at_quantile(0.5), 2_000_000);
        assert_within_three_significant_figures(merged_histogram.max(), 4_000_000);
    }

    /// The interval log stores histograms base64'd; decoding here rather than
    /// depending on a base64 crate keeps the spike's dependency set as declared.
    fn base64_decode_histogram(encoded: &str) -> Vec<u8> {
        const BASE64_ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut accumulator: u32 = 0;
        let mut accumulated_bits: u32 = 0;
        let mut decoded_bytes = Vec::new();
        for encoded_byte in encoded.bytes() {
            if encoded_byte == b'=' {
                break;
            }
            let sextet = BASE64_ALPHABET
                .iter()
                .position(|&alphabet_byte| alphabet_byte == encoded_byte)
                .expect("interval log payloads are standard base64")
                as u32;
            accumulator = (accumulator << 6) | sextet;
            accumulated_bits += 6;
            if accumulated_bits >= 8 {
                accumulated_bits -= 8;
                decoded_bytes.push((accumulator >> accumulated_bits) as u8);
            }
        }
        decoded_bytes
    }
}
