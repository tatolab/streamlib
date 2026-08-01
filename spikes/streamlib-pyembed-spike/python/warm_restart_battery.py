#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Gate 6 of #1702: how long a restart takes to put a frame on the wire.

One cold run followed by N warm ones, each a fresh process, measuring
exec-to-first-frame: a `CLOCK_MONOTONIC` stamp taken immediately before spawn
against the stamp the harness's sink records when it sees its first frame. Both
sides read the same clock on the same machine, so no offset estimation is
involved and nothing depends on the harness reporting its own start time
honestly.

Why the cold run is separate rather than dropped: it is the only one that pays
the page cache, and reporting it inside the median would understate the warm
figure the dev loop actually feels while overstating the first-run figure a user
hits. Both are reported; only the warm ones back the gate.

The battery forces `--startup-settle-seconds 0`. The settle exists so latency
cells measure a quiescent graph; here it would simply be added to every number.

Gate 6 as amended asks for `import torch` reported separately. `--extra-import`
takes any module and reports its import cost in this interpreter, so the number
is attributable rather than folded into the harness's own startup.
"""

from __future__ import annotations

import argparse
import importlib
import json
import os
import statistics
import subprocess
import sys
import time

import runner

# Gate 6's threshold: warm restart at or under this median backs a GO.
WARM_RESTART_MEDIAN_GATE_SECONDS = 1.5
# Above this, #1702 calls the execution pivot dead outright.
WARM_RESTART_NO_GO_SECONDS = 5.0

# Long enough to be certain a frame lands and the cell writes its summary,
# short enough that eleven runs stay quick. The battery measures startup, so
# nothing past the first frame matters.
BATTERY_CELL_DURATION_SECONDS = 3
BATTERY_FRAME_WIDTH_PIXELS = 1280
BATTERY_FRAME_HEIGHT_PIXELS = 720


class RestartRunProducedNoFrameError(RuntimeError):
    """Raised when a run finished without the sink ever seeing a frame.

    Reported rather than skipped: a battery that quietly dropped such runs
    would report the median of whichever runs happened to work.
    """


def run_one_restart_and_measure_seconds(
    harness_binary_path: str,
    harness_arm: str,
    output_directory: str,
    repetition_index: int,
) -> float:
    """Spawn one harness process and return its exec-to-first-frame seconds."""
    harness_command_line = [
        harness_binary_path,
        "--arm",
        harness_arm,
        "--fps",
        "60",
        "--frame-width",
        str(BATTERY_FRAME_WIDTH_PIXELS),
        "--frame-height",
        str(BATTERY_FRAME_HEIGHT_PIXELS),
        "--duration-seconds",
        str(BATTERY_CELL_DURATION_SECONDS),
        "--warmup-exclusion-seconds",
        "0",
        "--startup-settle-seconds",
        "0",
        "--repetition-index",
        str(repetition_index),
        "--output-directory",
        output_directory,
    ]

    spawned_at_epoch_seconds = time.time()
    # Taken as late as possible: everything between this stamp and `exec` is
    # charged to the restart, which is the honest direction for a startup gate.
    spawned_at_monotonic_nanoseconds = time.monotonic_ns()
    completed_process = subprocess.run(
        harness_command_line,
        env=runner.build_tier_a_harness_process_environment(),
        check=False,
        capture_output=True,
        text=True,
    )
    if completed_process.returncode != 0:
        raise RestartRunProducedNoFrameError(
            f"restart run {repetition_index} exited {completed_process.returncode}:\n"
            f"{completed_process.stderr[-2000:]}"
        )

    cell_directory = runner.locate_measurement_cell_directory_created_by_harness(
        output_directory, spawned_at_epoch_seconds
    )
    if cell_directory is None:
        raise RestartRunProducedNoFrameError(
            f"restart run {repetition_index} left no cell directory under "
            f"{output_directory}"
        )
    with open(
        os.path.join(cell_directory, runner.HARNESS_CELL_SUMMARY_FILE_NAME)
    ) as summary_file:
        summary = json.load(summary_file)

    first_frame_monotonic_nanoseconds = summary[
        "first_frame_sink_receive_monotonic_nanoseconds"
    ]
    if first_frame_monotonic_nanoseconds is None:
        raise RestartRunProducedNoFrameError(
            f"restart run {repetition_index} completed but its sink never saw a frame"
        )
    return (
        first_frame_monotonic_nanoseconds - spawned_at_monotonic_nanoseconds
    ) / 1e9


def measure_extra_import_seconds(module_name: str) -> float:
    """Import cost of `module_name` in a fresh interpreter, reported separately."""
    probe = subprocess.run(
        [
            sys.executable,
            "-c",
            "import time;"
            "start=time.monotonic_ns();"
            f"__import__({module_name!r});"
            "print((time.monotonic_ns()-start)/1e9)",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    return float(probe.stdout.strip())


def run_warm_restart_battery(
    harness_binary_path: str,
    harness_arm: str,
    output_directory: str,
    warm_run_count: int,
    extra_import_module_names: list[str],
) -> dict:
    os.makedirs(output_directory, exist_ok=True)
    cold_restart_seconds = run_one_restart_and_measure_seconds(
        harness_binary_path, harness_arm, output_directory, 0
    )
    warm_restart_seconds = [
        run_one_restart_and_measure_seconds(
            harness_binary_path, harness_arm, output_directory, index + 1
        )
        for index in range(warm_run_count)
    ]

    warm_restart_median_seconds = statistics.median(warm_restart_seconds)
    return {
        "harness_arm": harness_arm,
        "cold_restart_seconds": cold_restart_seconds,
        "warm_restart_seconds": warm_restart_seconds,
        "warm_restart_median_seconds": warm_restart_median_seconds,
        "warm_restart_maximum_seconds": max(warm_restart_seconds),
        "gate_6_median_threshold_seconds": WARM_RESTART_MEDIAN_GATE_SECONDS,
        "gate_6_passes": warm_restart_median_seconds
        <= WARM_RESTART_MEDIAN_GATE_SECONDS,
        "no_go_threshold_seconds": WARM_RESTART_NO_GO_SECONDS,
        "exceeds_no_go_threshold": warm_restart_median_seconds
        > WARM_RESTART_NO_GO_SECONDS,
        "extra_import_seconds": {
            module_name: measure_extra_import_seconds(module_name)
            for module_name in extra_import_module_names
        },
    }


def main() -> int:
    argument_parser = argparse.ArgumentParser(description=__doc__)
    argument_parser.add_argument(
        "--harness-binary",
        default=os.environ.get(runner.HARNESS_BINARY_ENVIRONMENT_VARIABLE),
        required=False,
    )
    argument_parser.add_argument(
        "--arm",
        default="in-process-python",
        help="which arm restarts; the gate is about the in-process one, and the "
        "subprocess arm is available for comparison",
    )
    argument_parser.add_argument("--warm-run-count", type=int, default=10)
    argument_parser.add_argument(
        "--extra-import",
        action="append",
        default=[],
        metavar="MODULE",
        help="report this module's import cost separately (gate 6 names torch)",
    )
    argument_parser.add_argument("--output-dir", required=True)
    arguments = argument_parser.parse_args()

    if not arguments.harness_binary or not os.path.isfile(arguments.harness_binary):
        sys.stderr.write(
            "no harness binary; pass --harness-binary or set "
            f"${runner.HARNESS_BINARY_ENVIRONMENT_VARIABLE}\n"
        )
        return 2

    battery_record = run_warm_restart_battery(
        arguments.harness_binary,
        arguments.arm,
        arguments.output_dir,
        arguments.warm_run_count,
        arguments.extra_import,
    )
    with open(
        os.path.join(arguments.output_dir, "warm-restart-battery.json"), "w"
    ) as record_file:
        json.dump(battery_record, record_file, indent=2)
    sys.stdout.write(json.dumps(battery_record, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
