#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Evaluate #1702's gates over a directory of measurement cells.

The rule this is built around: **never report a verdict a cell cannot support.**
A gate is evaluated only when every cell backing it is admissible and its
threshold is actually stated. Anything else is reported as NOT EVALUATED with
the reason, because a silently-skipped gate reads as a passed one.

Four ways a cell is inadmissible, all recorded in its own artifacts:

* `measurement_stamping_is_compiled_in: false` — a control build whose only
  valid output is throughput.
* `wire_payload_mode: full-pixel-payload` — the retracted payload sweep, which
  measures transport size rather than the hosting question.
* `negative_latency_anomaly_count` or `histogram_range_saturation_count`
  nonzero — clocks disagreed, or percentiles are clipped floors.
* `backlog_drain_fraction` past its bound — the cell was draining a startup
  queue, so its percentiles describe occupancy and vary with cell duration.

Two of the amended gates carry no stated threshold. Owner decision 3 added a
floor-vs-PyO3 delta gate and decision 4 added an absolute p99.9 ceiling, but
neither names a number, and #1702 is explicit that thresholds are evaluated
verbatim and never decided inline. Both are computed and reported; both stay
NOT EVALUATED until `--floor-delta-gate-ms` / `--absolute-p99-9-ceiling-ms` are
passed. That is a deliberate refusal, not an omission.
"""

from __future__ import annotations

import argparse
import glob
import json
import os
import sys

ARM_IN_PROCESS_PYTHON = "in-process-python"
ARM_RUST_PASSTHROUGH_FLOOR = "rust-passthrough-floor"
ARM_SUBPROCESS_PYTHON_BASELINE = "subprocess-python-baseline"

# Owner decision 1: latency percentiles are primary, so a cell must have been
# wired for every-sample delivery. Under `latest` the sink drains to the newest
# frame and the percentiles are pinned near one frame period whatever happens.
REQUIRED_DELIVERY_PROFILE = "every_sample"

# Matches the Rust harness's own invalidation bound. Applied to the absolute
# value: a cell whose latency *grew* across its life is saturating, which is as
# disqualifying as one that was draining.
BACKLOG_TREND_FRACTION_INVALIDATING_A_CELL = 0.20

# A pipeline whose per-frame service time reaches its frame period cannot keep
# up, so its percentiles describe queue occupancy rather than latency. This is
# the check that catches a *steady* backlog, which no trend detector can see:
# with the settle removed, a subprocess cell sat at p50 66.9ms with a trend of
# -0.002 — a stable queue, invisible to the decile comparison, and the
# summarizer passed gate 1 against it at 0.127ms vs 66.9ms.
SATURATION_FRAME_PERIOD_MULTIPLE_INVALIDATING_A_CELL = 1.0

# Owner decision: cells must be measured from a quiescent graph. A cell run with
# a shorter settle than its peers is not comparable to them.
PROTOCOL_STARTUP_SETTLE_SECONDS = 2.0

# Gate 2's stated absolute ceilings, by target rate.
GATE_2_ABSOLUTE_CEILING_MILLISECONDS_BY_RATE = {60: 50.0, 30: 100.0}
GATE_2_BASELINE_HEADROOM_MILLISECONDS = 2.0
# Gate 5, demoted to a sanity check by owner decision 3.
GATE_5_CALLBACK_SANITY_MILLISECONDS = 0.5

NANOSECONDS_PER_MILLISECOND = 1e6


def load_measurement_cells(matrix_directory: str) -> list[dict]:
    """Every cell under `matrix_directory`, as its summary plus its path."""
    cells = []
    for summary_path in sorted(
        glob.glob(os.path.join(matrix_directory, "*", "summary.json"))
    ):
        with open(summary_path) as summary_file:
            cells.append(
                {
                    "directory": os.path.dirname(summary_path),
                    "summary": json.load(summary_file),
                }
            )
    return cells


def describe_cell_inadmissibility(cell: dict) -> str | None:
    """Why this cell cannot back a gated number, or None when it can."""
    summary = cell["summary"]
    specification = summary["specification"]

    if not specification.get("measurement_stamping_is_compiled_in", True):
        return "built with stamping compiled out; only its throughput is meaningful"
    if specification.get("wire_payload_mode") != "surface-reference":
        return (
            "carries "
            f"{specification.get('wire_payload_mode')!r} on the wire; the retracted "
            "payload sweep measures transport size, not the hosting question"
        )
    if specification.get("resolved_delivery_profile") != REQUIRED_DELIVERY_PROFILE:
        return (
            "resolved delivery profile "
            f"{specification.get('resolved_delivery_profile')!r}; latency percentiles "
            f"require {REQUIRED_DELIVERY_PROFILE!r} (owner decision 1)"
        )
    if summary.get("received_frame_count", 0) == 0:
        return "received no frames"
    if summary.get("negative_latency_anomaly_count", 0):
        return "recorded sink stamps earlier than their emit stamps; the clocks disagree"
    if summary.get("histogram_range_saturation_count", 0):
        return "saturated the histogram range; its top percentiles are floors, not values"
    backlog_trend = summary.get("backlog_drain_fraction", 0.0)
    if abs(backlog_trend) > BACKLOG_TREND_FRACTION_INVALIDATING_A_CELL:
        direction = "shed" if backlog_trend > 0 else "gained"
        return (
            f"{direction} {abs(backlog_trend):.0%} of its latency across its own life; "
            "it was draining or building a queue, so its percentiles describe queue "
            "occupancy and depend on how long it ran"
        )

    frame_period_milliseconds = 1000.0 / specification["target_frames_per_second"]
    p50_milliseconds = (
        summary["source_emit_to_sink_receive"]["p50_nanoseconds"]
        / NANOSECONDS_PER_MILLISECOND
    )
    saturation_ceiling = (
        frame_period_milliseconds * SATURATION_FRAME_PERIOD_MULTIPLE_INVALIDATING_A_CELL
    )
    if p50_milliseconds >= saturation_ceiling:
        return (
            f"ran at p50 {p50_milliseconds:.3f}ms against a {frame_period_milliseconds:.3f}ms "
            "frame period; per-frame service time reached the frame period, so the "
            "pipeline was saturated and these percentiles describe queue occupancy"
        )

    settle = specification.get("startup_settle_seconds")
    if settle is None or settle < PROTOCOL_STARTUP_SETTLE_SECONDS:
        return (
            f"ran with a {settle}s startup settle, below the protocol's "
            f"{PROTOCOL_STARTUP_SETTLE_SECONDS}s; frames emitted before the graph was "
            "quiescent queue up and the backlog outlives the warmup exclusion"
        )
    return None


def build_cell_condition_key(cell: dict) -> tuple:
    """Everything but the arm — the cells that must be compared against each other."""
    specification = cell["summary"]["specification"]
    return (
        specification["frame_width_pixels"],
        specification["frame_height_pixels"],
        specification["target_frames_per_second"],
        specification["stage_callback_attribute"],
        # The module too, not just the attribute: it became a per-cell knob in
        # the same change that added the settle, so two cells running
        # same-named callables from different modules would otherwise be
        # compared head-to-head.
        specification.get("stage_callback_module"),
        specification["anchor_processor_thread_gil"],
        # Part of the key, not just the admissibility check: arms measured under
        # different settles are not comparable, and omitting it let a settle-0
        # baseline be compared head-to-head with settle-2 in-process cells.
        specification.get("startup_settle_seconds"),
    )


def median_of(values: list[float]) -> float:
    ordered = sorted(values)
    middle = len(ordered) // 2
    if len(ordered) % 2:
        return ordered[middle]
    return (ordered[middle - 1] + ordered[middle]) / 2


def summarize_arm_across_repetitions(cells: list[dict]) -> dict:
    """Median of each percentile across an arm's repetitions at one condition.

    Median rather than mean: the distribution is heavily tailed, and one bad
    repetition should not move the arm's reported figure.
    """
    def median_percentile(percentile_key: str) -> float:
        return median_of(
            [
                cell["summary"]["source_emit_to_sink_receive"][percentile_key]
                / NANOSECONDS_PER_MILLISECOND
                for cell in cells
            ]
        )

    return {
        "repetition_count": len(cells),
        "p50_ms": median_percentile("p50_nanoseconds"),
        "p99_ms": median_percentile("p99_nanoseconds"),
        "p99_9_ms": median_percentile("p99_9_nanoseconds"),
        "max_ms": median_percentile("max_nanoseconds"),
        "stage_callback_p99_ms": median_of(
            [
                cell["summary"]["stage_callback"]["p99_nanoseconds"]
                / NANOSECONDS_PER_MILLISECOND
                for cell in cells
            ]
        ),
        "dropped_frame_count": sum(
            cell["summary"]["dropped_frame_count"] for cell in cells
        ),
        "worst_rolling_frame_rate": min(
            (
                min(cell["summary"]["rolling_one_second_frame_rate_windows"])
                for cell in cells
                if cell["summary"]["rolling_one_second_frame_rate_windows"]
            ),
            default=None,
        ),
    }


def evaluate_condition(
    condition_key: tuple,
    arms: dict,
    floor_delta_gate_milliseconds: float | None,
    absolute_p99_9_ceiling_milliseconds: float | None,
) -> dict:
    """Evaluate every gate this condition's admissible cells can support."""
    (
        width,
        height,
        frames_per_second,
        stage,
        stage_module,
        gil_anchor,
        startup_settle,
    ) = condition_key
    verdicts: dict[str, dict] = {}

    def record(gate: str, passed: bool | None, detail: str) -> None:
        verdicts[gate] = {
            "verdict": "NOT EVALUATED" if passed is None else ("PASS" if passed else "FAIL"),
            "detail": detail,
        }

    in_process = arms.get(ARM_IN_PROCESS_PYTHON)
    baseline = arms.get(ARM_SUBPROCESS_PYTHON_BASELINE)
    floor = arms.get(ARM_RUST_PASSTHROUGH_FLOOR)

    if in_process is None or baseline is None:
        missing = [
            name
            for name, arm in (
                (ARM_IN_PROCESS_PYTHON, in_process),
                (ARM_SUBPROCESS_PYTHON_BASELINE, baseline),
            )
            if arm is None
        ]
        for gate in ("gate_1_p50", "gate_2_p99", "gate_3_p99_9"):
            record(gate, None, f"no admissible cells for {', '.join(missing)}")
    else:
        record(
            "gate_1_p50",
            in_process["p50_ms"] < baseline["p50_ms"],
            f"in-process p50 {in_process['p50_ms']:.3f}ms vs baseline "
            f"{baseline['p50_ms']:.3f}ms",
        )

        absolute_ceiling = GATE_2_ABSOLUTE_CEILING_MILLISECONDS_BY_RATE.get(
            frames_per_second
        )
        if absolute_ceiling is None:
            record(
                "gate_2_p99",
                None,
                f"#1702 states an absolute p99 ceiling for 30 and 60fps only, not "
                f"{frames_per_second}fps",
            )
        else:
            record(
                "gate_2_p99",
                in_process["p99_ms"]
                <= baseline["p99_ms"] + GATE_2_BASELINE_HEADROOM_MILLISECONDS
                and in_process["p99_ms"] <= absolute_ceiling,
                f"in-process p99 {in_process['p99_ms']:.3f}ms vs baseline+2ms "
                f"{baseline['p99_ms'] + GATE_2_BASELINE_HEADROOM_MILLISECONDS:.3f}ms "
                f"and absolute {absolute_ceiling:.1f}ms",
            )

        one_frame_period_milliseconds = 1000.0 / frames_per_second
        relative_p99_9_passes = (
            in_process["p99_9_ms"] <= baseline["p99_9_ms"] + one_frame_period_milliseconds
        )
        if absolute_p99_9_ceiling_milliseconds is None:
            record(
                "gate_3_p99_9",
                None,
                "owner decision 4 added an absolute p99.9 ceiling but states no "
                "number; pass --absolute-p99-9-ceiling-ms to evaluate. Relative "
                f"half would {'pass' if relative_p99_9_passes else 'fail'}: "
                f"in-process p99.9 {in_process['p99_9_ms']:.3f}ms vs baseline+one "
                f"frame period "
                f"{baseline['p99_9_ms'] + one_frame_period_milliseconds:.3f}ms",
            )
        else:
            record(
                "gate_3_p99_9",
                relative_p99_9_passes
                and in_process["p99_9_ms"] <= absolute_p99_9_ceiling_milliseconds,
                f"in-process p99.9 {in_process['p99_9_ms']:.3f}ms vs baseline+one "
                f"frame period "
                f"{baseline['p99_9_ms'] + one_frame_period_milliseconds:.3f}ms and "
                f"absolute {absolute_p99_9_ceiling_milliseconds:.3f}ms",
            )

    if in_process is None:
        record("gate_4_frame_rate_stability", None, "no admissible in-process cells")
        record("gate_5_callback_sanity", None, "no admissible in-process cells")
    else:
        worst_window = in_process["worst_rolling_frame_rate"]
        if worst_window is None:
            record(
                "gate_4_frame_rate_stability",
                None,
                "no cell was long enough to produce a full rolling 1s window",
            )
        else:
            # Only the first of gate 4's four clauses. Reported under its own
            # name so the other three are visibly absent rather than folded
            # into a bare PASS for the whole gate.
            record(
                "gate_4a_rolling_frame_rate_floor",
                worst_window >= frames_per_second - 1,
                f"worst rolling 1s window {worst_window:.2f}fps against a "
                f"{frames_per_second - 1}fps floor; "
                f"{in_process['dropped_frame_count']} drops (reported, not gated — "
                "owner decision 1)",
            )
        record(
            "gate_4b_sub_target_windows_are_gc_attributable",
            None,
            "not implemented: needs the sub-target windows joined against "
            "gc-collections-embedded-interpreter.jsonl, which this evaluator does "
            "not read",
        )
        record(
            "gate_4c_soak_shows_no_monotonic_growth",
            None,
            "not implemented: needs a soak cell and an RSS series, neither of which "
            "this PR produces",
        )
        record(
            "gate_5_callback_sanity",
            in_process["stage_callback_p99_ms"] <= GATE_5_CALLBACK_SANITY_MILLISECONDS,
            f"callback p99 {in_process['stage_callback_p99_ms']:.4f}ms against "
            f"{GATE_5_CALLBACK_SANITY_MILLISECONDS}ms — demoted to a sanity check by "
            "owner decision 3, never a NO-GO on its own",
        )

    if in_process is None or floor is None:
        record("floor_delta", None, "needs both an in-process and a floor arm")
    else:
        floor_delta_milliseconds = in_process["p50_ms"] - floor["p50_ms"]
        detail = (
            f"PyO3 costs {floor_delta_milliseconds:.4f}ms at p50 over the pure-Rust "
            f"floor ({in_process['p50_ms']:.3f}ms vs {floor['p50_ms']:.3f}ms)"
        )
        if floor_delta_gate_milliseconds is None:
            record(
                "floor_delta",
                None,
                f"{detail}. Owner decision 3 made this the gate but states no "
                "threshold; pass --floor-delta-gate-ms to evaluate",
            )
        else:
            record(
                "floor_delta",
                floor_delta_milliseconds <= floor_delta_gate_milliseconds,
                f"{detail}, against {floor_delta_gate_milliseconds:.4f}ms",
            )

    return {
        "condition": {
            "frame_width_pixels": width,
            "frame_height_pixels": height,
            "target_frames_per_second": frames_per_second,
            "stage_callback_attribute": stage,
            "stage_callback_module": stage_module,
            "anchor_processor_thread_gil": gil_anchor,
            "startup_settle_seconds": startup_settle,
        },
        "arms": arms,
        "gates": verdicts,
    }


def evaluate_warm_restart_battery(matrix_directory: str) -> dict:
    """Gate 6, read from the battery's own record when one is present.

    Folded into this report rather than left in a separate file: the GO decision
    is read from one place, and a gate that lives somewhere else reads as a gate
    nobody measured.
    """
    battery_path = os.path.join(matrix_directory, "warm-restart-battery.json")
    if not os.path.isfile(battery_path):
        return {
            "verdict": "NOT EVALUATED",
            "detail": f"no warm-restart-battery.json under {matrix_directory}; run "
            "python/warm_restart_battery.py",
        }
    with open(battery_path) as battery_file:
        battery = json.load(battery_file)
    return {
        "verdict": "PASS" if battery["gate_6_passes"] else "FAIL",
        "detail": f"warm restart median {battery['warm_restart_median_seconds']:.3f}s "
        f"against {battery['gate_6_median_threshold_seconds']}s "
        f"(first run {battery['first_restart_seconds']:.3f}s, "
        f"arm {battery['harness_arm']})",
    }


def summarize_measurement_matrix(
    matrix_directory: str,
    floor_delta_gate_milliseconds: float | None,
    absolute_p99_9_ceiling_milliseconds: float | None,
) -> dict:
    cells = load_measurement_cells(matrix_directory)
    admissible: list[dict] = []
    excluded: list[dict] = []
    for cell in cells:
        reason = describe_cell_inadmissibility(cell)
        if reason is None:
            admissible.append(cell)
        else:
            excluded.append(
                {"directory": os.path.basename(cell["directory"]), "reason": reason}
            )

    cells_by_condition_and_arm: dict[tuple, dict[str, list]] = {}
    for cell in admissible:
        condition = cells_by_condition_and_arm.setdefault(
            build_cell_condition_key(cell), {}
        )
        condition.setdefault(cell["summary"]["specification"]["arm"], []).append(cell)

    conditions = [
        evaluate_condition(
            condition_key,
            {
                arm: summarize_arm_across_repetitions(arm_cells)
                for arm, arm_cells in arms.items()
            },
            floor_delta_gate_milliseconds,
            absolute_p99_9_ceiling_milliseconds,
        )
        for condition_key, arms in sorted(cells_by_condition_and_arm.items())
    ]

    warm_restart_gate = evaluate_warm_restart_battery(matrix_directory)
    every_verdict = [
        verdict["verdict"]
        for condition in conditions
        for verdict in condition["gates"].values()
    ] + [warm_restart_gate["verdict"]]
    return {
        "matrix_directory": matrix_directory,
        "cell_count": len(cells),
        "admissible_cell_count": len(admissible),
        "excluded_cells": excluded,
        "conditions": conditions,
        "gate_6_warm_restart": warm_restart_gate,
        "gate_tally": {
            "pass": every_verdict.count("PASS"),
            "fail": every_verdict.count("FAIL"),
            "not_evaluated": every_verdict.count("NOT EVALUATED"),
        },
    }


def render_report(matrix_summary: dict) -> str:
    lines = [
        f"#1702 gate evaluation over {matrix_summary['matrix_directory']}",
        f"  {matrix_summary['admissible_cell_count']} of "
        f"{matrix_summary['cell_count']} cells admissible",
        "",
    ]
    for excluded in matrix_summary["excluded_cells"]:
        lines.append(f"  EXCLUDED {excluded['directory']}")
        lines.append(f"           {excluded['reason']}")
    if matrix_summary["excluded_cells"]:
        lines.append("")

    for condition in matrix_summary["conditions"]:
        c = condition["condition"]
        lines.append(
            f"{c['frame_width_pixels']}x{c['frame_height_pixels']} @ "
            f"{c['target_frames_per_second']}fps  stage={c['stage_callback_attribute']}  "
            f"gil-anchor={'on' if c['anchor_processor_thread_gil'] else 'off'}"
        )
        for arm_name, arm in sorted(condition["arms"].items()):
            lines.append(
                f"  {arm_name:28} p50 {arm['p50_ms']:8.3f}ms  p99 {arm['p99_ms']:8.3f}ms  "
                f"p99.9 {arm['p99_9_ms']:8.3f}ms  (n={arm['repetition_count']})"
            )
        for gate_name, gate in condition["gates"].items():
            lines.append(f"  [{gate['verdict']:>13}] {gate_name}")
            lines.append(f"                  {gate['detail']}")
        lines.append("")

    warm_restart_gate = matrix_summary["gate_6_warm_restart"]
    lines.append(f"  [{warm_restart_gate['verdict']:>13}] gate_6_warm_restart")
    lines.append(f"                  {warm_restart_gate['detail']}")
    lines.append("")

    tally = matrix_summary["gate_tally"]
    lines.append(
        f"{tally['pass']} pass, {tally['fail']} fail, "
        f"{tally['not_evaluated']} not evaluated"
    )
    return "\n".join(lines)


def main() -> int:
    argument_parser = argparse.ArgumentParser(description=__doc__)
    argument_parser.add_argument("matrix_directory")
    argument_parser.add_argument(
        "--floor-delta-gate-ms",
        type=float,
        default=None,
        help="threshold for the floor-vs-PyO3 p50 delta gate (owner decision 3 "
        "added the gate but stated no number)",
    )
    argument_parser.add_argument(
        "--absolute-p99-9-ceiling-ms",
        type=float,
        default=None,
        help="absolute p99.9 ceiling (owner decision 4 added it but stated no number)",
    )
    argument_parser.add_argument("--json-out", default=None)
    arguments = argument_parser.parse_args()

    matrix_summary = summarize_measurement_matrix(
        arguments.matrix_directory,
        arguments.floor_delta_gate_ms,
        arguments.absolute_p99_9_ceiling_ms,
    )
    if arguments.json_out:
        with open(arguments.json_out, "w") as json_file:
            json.dump(matrix_summary, json_file, indent=2)
    sys.stdout.write(render_report(matrix_summary) + "\n")
    # A failing gate is a result, not a harness error, so the exit status
    # distinguishes them: 0 for "evaluated", 1 for "a gate failed".
    return 1 if matrix_summary["gate_tally"]["fail"] else 0


if __name__ == "__main__":
    sys.exit(main())
