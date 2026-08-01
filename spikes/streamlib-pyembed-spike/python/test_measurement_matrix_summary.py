# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Tests for the gate evaluator.

The property under test throughout is the one the evaluator exists for: a gate
it cannot honestly evaluate must come out NOT EVALUATED, never PASS. A silently
skipped gate reads as a passed one, and this artifact is what a GO decision
rests on."""

import json
import os
import tempfile
import unittest

import summarize_measurement_matrix as summarizer


def build_cell_summary(
    arm,
    p50_nanoseconds=100_000,
    p99_nanoseconds=140_000,
    p99_9_nanoseconds=180_000,
    stage_callback_p99_nanoseconds=20_000,
    frames_per_second=60,
    wire_payload_mode="surface-reference",
    delivery_profile="every_sample",
    stamping_compiled_in=True,
    received_frame_count=900,
    negative_latency_anomaly_count=0,
    histogram_range_saturation_count=0,
    backlog_drain_fraction=0.0,
    startup_settle_seconds=2.0,
    rolling_windows=None,
):
    return {
        "specification": {
            "arm": arm,
            "frame_width_pixels": 1280,
            "frame_height_pixels": 720,
            "channel_count": 4,
            "target_frames_per_second": frames_per_second,
            "wire_payload_mode": wire_payload_mode,
            "startup_settle_seconds": startup_settle_seconds,
            "stage_callback_attribute": "passthrough_stage",
            "anchor_processor_thread_gil": True,
            "resolved_delivery_profile": delivery_profile,
            "measured_metric_name": "source_emit_to_sink_receive",
            "measurement_stamping_is_compiled_in": stamping_compiled_in,
        },
        "source_emit_to_sink_receive": {
            "p50_nanoseconds": p50_nanoseconds,
            "p99_nanoseconds": p99_nanoseconds,
            "p99_9_nanoseconds": p99_9_nanoseconds,
            "max_nanoseconds": p99_9_nanoseconds,
        },
        "stage_callback": {
            "p50_nanoseconds": stage_callback_p99_nanoseconds // 2,
            "p99_nanoseconds": stage_callback_p99_nanoseconds,
            "p99_9_nanoseconds": stage_callback_p99_nanoseconds,
            "max_nanoseconds": stage_callback_p99_nanoseconds,
        },
        "received_frame_count": received_frame_count,
        "measured_frame_count": received_frame_count,
        "dropped_frame_count": 0,
        "negative_latency_anomaly_count": negative_latency_anomaly_count,
        "histogram_range_saturation_count": histogram_range_saturation_count,
        "backlog_drain_fraction": backlog_drain_fraction,
        "first_frame_sink_receive_monotonic_nanoseconds": 1_000,
        "rolling_one_second_frame_rate_windows": (
            [60.0] * 10 if rolling_windows is None else rolling_windows
        ),
    }


def write_matrix(matrix_directory, named_summaries):
    for cell_name, summary in named_summaries.items():
        cell_directory = os.path.join(matrix_directory, cell_name)
        os.makedirs(cell_directory)
        with open(os.path.join(cell_directory, "summary.json"), "w") as summary_file:
            json.dump(summary, summary_file)


class CellAdmissibilityTest(unittest.TestCase):
    def test_a_well_formed_cell_is_admissible(self):
        self.assertIsNone(
            summarizer.describe_cell_inadmissibility(
                {"directory": "d", "summary": build_cell_summary("in-process-python")}
            )
        )

    def test_a_control_build_cell_cannot_back_a_gate(self):
        reason = summarizer.describe_cell_inadmissibility(
            {
                "directory": "d",
                "summary": build_cell_summary(
                    "in-process-python", stamping_compiled_in=False
                ),
            }
        )
        self.assertIn("stamping compiled out", reason)

    def test_a_full_pixel_payload_cell_cannot_back_a_gate(self):
        """It measures transport size, which is the retracted sweep, not the
        hosting question the pivot turns on."""
        reason = summarizer.describe_cell_inadmissibility(
            {
                "directory": "d",
                "summary": build_cell_summary(
                    "in-process-python", wire_payload_mode="full-pixel-payload"
                ),
            }
        )
        self.assertIn("payload sweep", reason)

    def test_a_latest_profile_cell_cannot_back_a_latency_gate(self):
        """Owner decision 1: under `latest` the sink drains to the newest frame
        and the percentiles are pinned near one frame period regardless."""
        reason = summarizer.describe_cell_inadmissibility(
            {
                "directory": "d",
                "summary": build_cell_summary(
                    "in-process-python", delivery_profile="latest"
                ),
            }
        )
        self.assertIn("every_sample", reason)

    def test_a_cell_that_drained_a_backlog_cannot_back_a_gate(self):
        reason = summarizer.describe_cell_inadmissibility(
            {
                "directory": "d",
                "summary": build_cell_summary(
                    "in-process-python", backlog_drain_fraction=0.4
                ),
            }
        )
        self.assertIn("queue", reason)

    def test_a_cell_whose_latency_grew_cannot_back_a_gate(self):
        """Saturation is as disqualifying as draining, and the harness's check
        was one-sided: a cell that gained 35% across its life was admitted."""
        reason = summarizer.describe_cell_inadmissibility(
            {
                "directory": "d",
                "summary": build_cell_summary(
                    "in-process-python", backlog_drain_fraction=-0.35
                ),
            }
        )
        self.assertIn("gained", reason)

    def test_a_saturated_cell_cannot_back_a_gate_even_with_a_flat_trend(self):
        """The case no trend detector can see. With the settle removed, a
        subprocess cell sat at p50 66.9ms with a trend of -0.002 — a stable
        queue — and gate 1 passed against it at 0.127ms vs 66.9ms."""
        reason = summarizer.describe_cell_inadmissibility(
            {
                "directory": "d",
                "summary": build_cell_summary(
                    "subprocess-python-baseline",
                    p50_nanoseconds=66_900_000,
                    p99_nanoseconds=71_600_000,
                    p99_9_nanoseconds=72_000_000,
                    backlog_drain_fraction=-0.002,
                ),
            }
        )
        self.assertIn("saturated", reason)

    def test_a_cell_run_below_the_protocol_settle_cannot_back_a_gate(self):
        reason = summarizer.describe_cell_inadmissibility(
            {
                "directory": "d",
                "summary": build_cell_summary(
                    "in-process-python", startup_settle_seconds=0.0
                ),
            }
        )
        self.assertIn("startup settle", reason)

    def test_a_clock_disagreement_cannot_back_a_gate(self):
        reason = summarizer.describe_cell_inadmissibility(
            {
                "directory": "d",
                "summary": build_cell_summary(
                    "in-process-python", negative_latency_anomaly_count=3
                ),
            }
        )
        self.assertIn("clocks disagree", reason)

    def test_a_saturated_histogram_cannot_back_a_gate(self):
        reason = summarizer.describe_cell_inadmissibility(
            {
                "directory": "d",
                "summary": build_cell_summary(
                    "in-process-python", histogram_range_saturation_count=1
                ),
            }
        )
        self.assertIn("floors, not values", reason)

    def test_an_empty_cell_cannot_back_a_gate(self):
        """Its percentiles are all zero, which reads as very fast rather than
        as nothing having arrived."""
        reason = summarizer.describe_cell_inadmissibility(
            {
                "directory": "d",
                "summary": build_cell_summary(
                    "in-process-python", received_frame_count=0
                ),
            }
        )
        self.assertIn("no frames", reason)


class GateEvaluationTest(unittest.TestCase):
    def summarize(self, named_summaries, **kwargs):
        with tempfile.TemporaryDirectory() as matrix_directory:
            write_matrix(matrix_directory, named_summaries)
            return summarizer.summarize_measurement_matrix(
                matrix_directory,
                kwargs.get("floor_delta_gate_milliseconds"),
                kwargs.get("absolute_p99_9_ceiling_milliseconds"),
            )

    def build_three_arm_matrix(self, **in_process_overrides):
        return {
            "floor": build_cell_summary(
                "rust-passthrough-floor",
                p50_nanoseconds=74_000,
                p99_nanoseconds=107_000,
                p99_9_nanoseconds=134_000,
                stage_callback_p99_nanoseconds=0,
            ),
            "in-process": build_cell_summary(
                "in-process-python", **in_process_overrides
            ),
            "baseline": build_cell_summary(
                "subprocess-python-baseline",
                p50_nanoseconds=167_000,
                p99_nanoseconds=236_000,
                p99_9_nanoseconds=296_000,
            ),
        }

    def gate(self, matrix_summary, gate_name):
        return matrix_summary["conditions"][0]["gates"][gate_name]

    def test_a_healthy_matrix_passes_every_stated_gate(self):
        matrix_summary = self.summarize(self.build_three_arm_matrix())
        for gate_name in (
            "gate_1_p50",
            "gate_2_p99",
            "gate_4a_rolling_frame_rate_floor",
            "gate_5_callback_sanity",
        ):
            self.assertEqual(
                self.gate(matrix_summary, gate_name)["verdict"],
                "PASS",
                f"{gate_name}: {self.gate(matrix_summary, gate_name)['detail']}",
            )

    def test_an_unstated_absolute_p99_9_ceiling_leaves_gate_3_unevaluated(self):
        """Owner decision 4 added the ceiling but named no number, and #1702 is
        explicit that thresholds are never decided inline. Reporting PASS on the
        relative half alone would silently narrow the gate."""
        gate = self.gate(self.summarize(self.build_three_arm_matrix()), "gate_3_p99_9")
        self.assertEqual(gate["verdict"], "NOT EVALUATED")
        self.assertIn("states no number", gate["detail"])

    def test_supplying_the_ceiling_evaluates_gate_3(self):
        gate = self.gate(
            self.summarize(
                self.build_three_arm_matrix(), absolute_p99_9_ceiling_milliseconds=1.0
            ),
            "gate_3_p99_9",
        )
        self.assertEqual(gate["verdict"], "PASS")

    def test_a_supplied_ceiling_can_fail_gate_3(self):
        gate = self.gate(
            self.summarize(
                self.build_three_arm_matrix(), absolute_p99_9_ceiling_milliseconds=0.01
            ),
            "gate_3_p99_9",
        )
        self.assertEqual(gate["verdict"], "FAIL")

    def test_an_unstated_floor_delta_threshold_leaves_that_gate_unevaluated(self):
        gate = self.gate(self.summarize(self.build_three_arm_matrix()), "floor_delta")
        self.assertEqual(gate["verdict"], "NOT EVALUATED")
        self.assertIn("0.0", gate["detail"])
        self.assertIn("states no threshold", gate["detail"])

    def test_a_slower_in_process_arm_fails_gate_1(self):
        matrix_summary = self.summarize(
            self.build_three_arm_matrix(p50_nanoseconds=900_000)
        )
        self.assertEqual(self.gate(matrix_summary, "gate_1_p50")["verdict"], "FAIL")

    def test_a_missing_baseline_arm_leaves_the_comparison_gates_unevaluated(self):
        """The baseline arm failing to run must never read as the in-process arm
        winning."""
        matrix = self.build_three_arm_matrix()
        del matrix["baseline"]
        matrix_summary = self.summarize(matrix)
        for gate_name in ("gate_1_p50", "gate_2_p99", "gate_3_p99_9"):
            self.assertEqual(
                self.gate(matrix_summary, gate_name)["verdict"], "NOT EVALUATED"
            )

    def test_an_unstated_rate_leaves_the_absolute_p99_ceiling_unevaluated(self):
        """#1702 states absolute p99 ceilings for 30 and 60fps only."""
        matrix = {
            "floor": build_cell_summary(
                "rust-passthrough-floor", frames_per_second=24
            ),
            "in-process": build_cell_summary(
                "in-process-python", frames_per_second=24
            ),
            "baseline": build_cell_summary(
                "subprocess-python-baseline",
                frames_per_second=24,
                p50_nanoseconds=167_000,
            ),
        }
        gate = self.gate(self.summarize(matrix), "gate_2_p99")
        self.assertEqual(gate["verdict"], "NOT EVALUATED")

    def test_a_dropped_frame_rate_window_fails_gate_4(self):
        matrix_summary = self.summarize(
            self.build_three_arm_matrix(rolling_windows=[60.0, 60.0, 51.0])
        )
        self.assertEqual(
            self.gate(matrix_summary, "gate_4a_rolling_frame_rate_floor")["verdict"], "FAIL"
        )

    def test_inadmissible_cells_are_reported_rather_than_dropped_silently(self):
        matrix = self.build_three_arm_matrix()
        matrix["sweep"] = build_cell_summary(
            "in-process-python", wire_payload_mode="full-pixel-payload"
        )
        matrix_summary = self.summarize(matrix)
        self.assertEqual(len(matrix_summary["excluded_cells"]), 1)
        self.assertEqual(matrix_summary["cell_count"], 4)
        self.assertEqual(matrix_summary["admissible_cell_count"], 3)

    def test_arms_are_summarized_by_median_across_repetitions(self):
        """A heavily-tailed distribution means one bad repetition must not move
        the arm's reported figure."""
        matrix = self.build_three_arm_matrix()
        matrix["in-process-rep-1"] = build_cell_summary(
            "in-process-python", p50_nanoseconds=100_000
        )
        matrix["in-process-rep-2"] = build_cell_summary(
            "in-process-python", p50_nanoseconds=9_000_000
        )
        matrix_summary = self.summarize(matrix)
        arm = matrix_summary["conditions"][0]["arms"]["in-process-python"]
        self.assertEqual(arm["repetition_count"], 3)
        self.assertAlmostEqual(arm["p50_ms"], 0.1)


if __name__ == "__main__":
    unittest.main()
