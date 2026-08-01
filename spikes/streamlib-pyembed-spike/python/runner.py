#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Drives the Rust Tier A harness over an A/B/A interleaved schedule: one
harness process per cell, a pure-Rust floor cell on either side of the arm under
test, so a drift in the machine between cells shows up as a floor-to-floor
difference instead of being charged to the arm under test.

The harness owns every measurement artifact of a cell, `cell-spec.json`
included. This runner records only what it resolved itself, into
`runner-invocation.json`, which points at the harness's cell directory.

Throwaway spike code. Its API shape is explicitly not a proposal for the SDK."""

import argparse
import json
import logging
import os
import subprocess
import sys
import time

import numpy

from gc_collection_attribution import (
    GARBAGE_COLLECTION_MODE_DEFAULT,
    GARBAGE_COLLECTION_MODE_TUNED,
    GarbageCollectionPauseAttributionRecorder,
    read_raw_monotonic_clock_nanoseconds,
)
from spike_stage_callbacks import build_measurement_stage_callback_for_stage_name

MEASUREMENT_CELL_LOGGER = logging.getLogger("streamlib_pyembed_spike.runner")

SPIKE_CRATE_ROOT_DIRECTORY = os.path.dirname(
    os.path.dirname(os.path.abspath(__file__))
)
SUBPROCESS_BASELINE_PACKAGE_NAME = "pyembed-subprocess-baseline"
APP_MODULES_DIRECTORY_ENVIRONMENT_VARIABLE = "STREAMLIB_MODULES_DIR"
PYTHON_NATIVE_LIBRARY_ENVIRONMENT_VARIABLE = "STREAMLIB_PYTHON_NATIVE_LIB"
BASELINE_VENV_PYTHON_ENVIRONMENT_VARIABLE = "STREAMLIB_BASELINE_VENV_PYTHON"

RUNNER_MODE_IN_PROCESS_PYTHON = "in-process"
RUNNER_MODE_SUBPROCESS_PYTHON = "subprocess"
RUNNER_MODE_RUST_PASSTHROUGH_FLOOR = "rust-floor"
SUPPORTED_RUNNER_MODES = (
    RUNNER_MODE_IN_PROCESS_PYTHON,
    RUNNER_MODE_SUBPROCESS_PYTHON,
    RUNNER_MODE_RUST_PASSTHROUGH_FLOOR,
)

# The runner's `--mode` spelling is the ticket's documented CLI; the harness's
# `--arm` spelling is what lands in the artifacts. They are deliberately
# separate vocabularies and this table is the only place they meet.
HARNESS_ARM_BY_RUNNER_MODE = {
    RUNNER_MODE_IN_PROCESS_PYTHON: "in-process-python",
    RUNNER_MODE_RUST_PASSTHROUGH_FLOOR: "rust-passthrough-floor",
    RUNNER_MODE_SUBPROCESS_PYTHON: "subprocess-python-baseline",
}

STAGE_NAME_PASSTHROUGH = "passthrough"
STAGE_NAME_REALISTIC = "realistic"
SUPPORTED_STAGE_NAMES = (STAGE_NAME_PASSTHROUGH, STAGE_NAME_REALISTIC)

# The harness imports this module from PYTHONPATH inside its embedded
# interpreter and calls the resolved attribute once per frame.
HARNESS_STAGE_CALLBACK_MODULE_NAME = "spike_stage_callbacks"
HARNESS_STAGE_CALLBACK_ATTRIBUTE_BY_STAGE_NAME = {
    STAGE_NAME_PASSTHROUGH: "passthrough_stage",
    STAGE_NAME_REALISTIC: "realistic_stage",
}

HARNESS_BINARY_ENVIRONMENT_VARIABLE = "STREAMLIB_PYEMBED_SPIKE_HARNESS_BINARY"

RUNNER_INVOCATION_RECORD_FILE_NAME = "runner-invocation.json"
HARNESS_CELL_SPECIFICATION_FILE_NAME = "cell-spec.json"
HARNESS_CELL_SUMMARY_FILE_NAME = "summary.json"
RUNNER_INTERPRETER_GARBAGE_COLLECTION_RECORD_FILE_NAME = (
    "gc-collections-runner-interpreter.jsonl"
)

FRAME_CHANNEL_COUNT = 4
STAGE_CALLBACK_CALIBRATION_ITERATION_COUNT = 200
STAGE_CALLBACK_CALIBRATION_WARMUP_ITERATION_COUNT = 10

# The protocol excludes the first 60s of every cell. A cell shorter than that
# cannot honour it, so short exploratory cells fall back to the value below and
# are logged as non-protocol rather than silently measuring nothing.
PROTOCOL_WARMUP_EXCLUSION_SECONDS = 60
EXPLORATORY_CELL_WARMUP_EXCLUSION_SECONDS = 1

# Some filesystems round modification times to whole seconds, so a cell
# directory written milliseconds after the harness started can carry a stamp
# fractionally before it. The newest match wins, so the slack only widens the
# candidate set.
CELL_DIRECTORY_MODIFICATION_TIME_SLACK_SECONDS = 2.0


class SubprocessBaselineArmIsNotProvisionedError(RuntimeError):
    """Raised when `--mode subprocess` runs before its provisioning step.

    The failure it replaces is silent and expensive: an unprovisioned slot
    fails at graph build, and a matrix run would record the baseline arm as
    absent rather than as unrunnable.
    """


def resolve_harness_arm_for_runner_mode(runner_mode):
    """Map a runner `--mode` to the harness `--arm` token it drives."""
    if runner_mode not in HARNESS_ARM_BY_RUNNER_MODE:
        raise ValueError(
            f"unsupported runner mode {runner_mode!r}; expected one of "
            f"{SUPPORTED_RUNNER_MODES}"
        )
    return HARNESS_ARM_BY_RUNNER_MODE[runner_mode]


def resolve_harness_stage_callback_attribute_for_stage_name(stage_name):
    """Map a runner `--stage` to the callable the harness imports per frame."""
    if stage_name not in HARNESS_STAGE_CALLBACK_ATTRIBUTE_BY_STAGE_NAME:
        raise ValueError(
            f"unsupported stage name {stage_name!r}; expected one of "
            f"{SUPPORTED_STAGE_NAMES}"
        )
    return HARNESS_STAGE_CALLBACK_ATTRIBUTE_BY_STAGE_NAME[stage_name]


def resolve_cell_duration_seconds(requested_duration_seconds):
    """The harness measures in whole seconds, so a fractional duration is a
    request the rig cannot honour and must not be rounded away silently."""
    whole_duration_seconds = int(requested_duration_seconds)
    if whole_duration_seconds != requested_duration_seconds:
        raise ValueError(
            f"--duration {requested_duration_seconds} is not a whole number of "
            "seconds; the harness's --duration-seconds is integral"
        )
    if whole_duration_seconds < 1:
        raise ValueError("--duration must be at least one second")
    return whole_duration_seconds


def resolve_warmup_exclusion_seconds(
    requested_warmup_exclusion_seconds, cell_duration_seconds
):
    """Resolve the warmup exclusion the harness is told to apply."""
    if requested_warmup_exclusion_seconds is not None:
        resolved_warmup_exclusion_seconds = requested_warmup_exclusion_seconds
    elif cell_duration_seconds > PROTOCOL_WARMUP_EXCLUSION_SECONDS:
        resolved_warmup_exclusion_seconds = PROTOCOL_WARMUP_EXCLUSION_SECONDS
    else:
        resolved_warmup_exclusion_seconds = EXPLORATORY_CELL_WARMUP_EXCLUSION_SECONDS
        MEASUREMENT_CELL_LOGGER.warning(
            "cell duration %ds cannot carry the protocol's %ds warmup exclusion; "
            "excluding %ds instead — these cells are exploratory and their "
            "percentiles are not protocol numbers",
            cell_duration_seconds,
            PROTOCOL_WARMUP_EXCLUSION_SECONDS,
            EXPLORATORY_CELL_WARMUP_EXCLUSION_SECONDS,
        )

    if resolved_warmup_exclusion_seconds < 0:
        raise ValueError("--warmup-exclusion-seconds cannot be negative")
    # A cell whose warmup covers its whole duration still exits 0 and still
    # writes a summary — one whose percentiles are all zero, which reads like a
    # very fast cell rather than an empty one.
    if resolved_warmup_exclusion_seconds >= cell_duration_seconds:
        raise ValueError(
            f"a {resolved_warmup_exclusion_seconds}s warmup exclusion covers the "
            f"whole {cell_duration_seconds}s cell and no frame would be measured; "
            "lengthen --duration or pass a smaller --warmup-exclusion-seconds"
        )
    return resolved_warmup_exclusion_seconds


def build_interleaved_measurement_schedule_for_runner_mode(runner_mode):
    """The cell order inside one repetition: floor, arm under test, floor.

    A schedule of only floor cells would compare the floor against itself, so
    the floor mode collapses to a single cell per repetition."""
    if runner_mode == RUNNER_MODE_RUST_PASSTHROUGH_FLOOR:
        return (RUNNER_MODE_RUST_PASSTHROUGH_FLOOR,)
    return (
        RUNNER_MODE_RUST_PASSTHROUGH_FLOOR,
        runner_mode,
        RUNNER_MODE_RUST_PASSTHROUGH_FLOOR,
    )


def build_harness_repetition_index(
    runner_repetition_index, schedule_position_index, schedule_length
):
    """Give every cell of every repetition its own harness repetition index.

    The harness derives its cell directory name from arm, rate, GIL anchor and
    repetition index — the two floor cells of one A/B/A repetition agree on all
    but the last, so sharing an index would make the second silently overwrite
    the first."""
    return runner_repetition_index * schedule_length + schedule_position_index


def build_tier_a_harness_command_line(
    command_line_arguments,
    harness_arm,
    harness_repetition_index,
    cell_duration_seconds,
    warmup_exclusion_seconds,
    garbage_collection_mode,
):
    """The exact argv for one cell. Every flag here is declared by
    `src/bin/tier_a_harness.rs`; the harness rejects anything else."""
    harness_command_line = [
        command_line_arguments.harness_binary,
        "--arm",
        harness_arm,
        "--fps",
        str(command_line_arguments.fps),
        "--frame-width",
        str(command_line_arguments.frame_width),
        "--frame-height",
        str(command_line_arguments.frame_height),
        "--channels",
        str(FRAME_CHANNEL_COUNT),
        "--duration-seconds",
        str(cell_duration_seconds),
        "--warmup-exclusion-seconds",
        str(warmup_exclusion_seconds),
        "--repetition-index",
        str(harness_repetition_index),
        "--stage-callback-module",
        HARNESS_STAGE_CALLBACK_MODULE_NAME,
        "--garbage-collection-mode",
        garbage_collection_mode,
        "--stage-callback-attribute",
        resolve_harness_stage_callback_attribute_for_stage_name(
            command_line_arguments.stage
        ),
        "--output-directory",
        os.path.abspath(command_line_arguments.output_dir),
    ]
    if command_line_arguments.disable_gil_anchor:
        harness_command_line.append("--disable-gil-anchor")
    if command_line_arguments.require_locked_measurement_state:
        harness_command_line.append("--require-locked-measurement-state")
    return harness_command_line


def build_tier_a_harness_process_environment():
    """The harness imports the stage callback module by name from PYTHONPATH, so
    this module's own directory has to be on it whatever the caller's cwd is."""
    harness_process_environment = dict(os.environ)
    spike_python_directory = os.path.dirname(os.path.abspath(__file__))
    existing_python_path = harness_process_environment.get("PYTHONPATH", "")
    harness_process_environment["PYTHONPATH"] = os.pathsep.join(
        [spike_python_directory, existing_python_path]
        if existing_python_path
        else [spike_python_directory]
    )
    harness_process_environment.update(read_subprocess_baseline_arm_environment())
    return harness_process_environment


def read_subprocess_baseline_arm_environment():
    """The two variables a subprocess cell needs, taken from the provisioning
    record rather than re-derived.

    Both are load-bearing and both fail silently. Without
    `STREAMLIB_MODULES_DIR` the loader looks for the package beside whatever
    the caller's cwd happens to be. Without `STREAMLIB_PYTHON_NATIVE_LIB` the
    spawned subprocess resolves *some other* `libstreamlib_python_native.so`,
    whose iceoryx2 service constants do not match the host's — the subprocess
    then fails to open its own input channel with
    `DoesNotSupportRequestedAmountOfPublishers`, the sink receives nothing, and
    the cell reads as an arm that produced no frames rather than one that was
    misconfigured. That is the failure #1702's finding 6 asked to be pinned
    against, and it only appears when the runner drives the cell, because a
    hand-run cell inherits the variable from the operator's shell.

    Returns an empty mapping when nothing has been provisioned; the guard in
    `require_subprocess_baseline_arm_is_provisioned` is what refuses the run.
    """
    provisioning_record_path = os.path.join(
        SPIKE_CRATE_ROOT_DIRECTORY, "target", "provisioned", "provisioning-record.json"
    )
    if not os.path.isfile(provisioning_record_path):
        return {}
    with open(provisioning_record_path) as provisioning_record_file:
        provisioning_record = json.load(provisioning_record_file)
    return {
        APP_MODULES_DIRECTORY_ENVIRONMENT_VARIABLE: provisioning_record[
            "application_modules_root"
        ],
        PYTHON_NATIVE_LIBRARY_ENVIRONMENT_VARIABLE: provisioning_record[
            "python_native_library_path"
        ],
        # Without this the machine-spec probe falls back to reporting the
        # interpreter as unknown rather than reporting `python3` on PATH as if
        # it were the one the subprocess arm launches.
        BASELINE_VENV_PYTHON_ENVIRONMENT_VARIABLE: provisioning_record[
            "package_venv_python"
        ],
    }


def require_subprocess_baseline_arm_is_provisioned():
    """Refuse a subprocess cell whose package slot was never provisioned.

    Checked here rather than left to the graph build so an unattended matrix
    run stops with the fix-it instead of recording the arm as absent.
    """
    venv_python = os.path.join(
        SPIKE_CRATE_ROOT_DIRECTORY,
        "target",
        "provisioned",
        SUBPROCESS_BASELINE_PACKAGE_NAME,
        ".venv",
        "bin",
        "python",
    )
    if not os.path.isfile(venv_python):
        raise SubprocessBaselineArmIsNotProvisionedError(
            "--mode subprocess needs its package provisioned first: no venv at "
            f"{venv_python}. Run "
            "`python3 python/provision_subprocess_baseline_package.py` from the "
            "spike crate root, then rerun."
        )


def locate_measurement_cell_directory_created_by_harness(
    harness_output_directory, harness_started_at_epoch_seconds
):
    """Find the cell directory the harness just created, by looking rather than
    predicting: the harness derives the name from its own specification and this
    runner must not carry a second copy of that rule."""
    if not os.path.isdir(harness_output_directory):
        return None
    earliest_acceptable_modification_time = (
        harness_started_at_epoch_seconds - CELL_DIRECTORY_MODIFICATION_TIME_SLACK_SECONDS
    )
    newest_cell_directory_path = None
    newest_summary_modification_time = None
    with os.scandir(harness_output_directory) as output_directory_entries:
        for output_directory_entry in output_directory_entries:
            if not output_directory_entry.is_dir():
                continue
            candidate_summary_path = os.path.join(
                output_directory_entry.path, HARNESS_CELL_SUMMARY_FILE_NAME
            )
            if not os.path.isfile(candidate_summary_path):
                continue
            summary_modification_time = os.stat(candidate_summary_path).st_mtime
            if summary_modification_time < earliest_acceptable_modification_time:
                continue
            if (
                newest_summary_modification_time is None
                or summary_modification_time > newest_summary_modification_time
            ):
                newest_summary_modification_time = summary_modification_time
                newest_cell_directory_path = output_directory_entry.path
    return newest_cell_directory_path


def summarize_sorted_nanosecond_samples(sorted_nanosecond_samples):
    """Percentile summary of already-sorted samples.

    No mean: the distribution is heavily tailed and a mean headline would hide
    exactly the GC and scheduler spikes this spike exists to find."""
    last_sample_index = len(sorted_nanosecond_samples) - 1

    def sample_at_percentile(percentile_fraction):
        sample_index = int(percentile_fraction * last_sample_index)
        return sorted_nanosecond_samples[sample_index]

    return {
        "sample_count": len(sorted_nanosecond_samples),
        "p50_nanoseconds": sample_at_percentile(0.50),
        "p99_nanoseconds": sample_at_percentile(0.99),
        "p99_9_nanoseconds": sample_at_percentile(0.999),
        "max_nanoseconds": sorted_nanosecond_samples[last_sample_index],
    }


def measure_stage_callback_cost_in_runner_interpreter(stage_name, frame_width, frame_height):
    """Time the harness's own stage callable against a synthetic frame, so the
    invocation record says what that stage costs on this machine.

    This runs in the runner's interpreter, not the harness's embedded one: it
    sizes the stage, it does not measure the cell."""
    measurement_stage_callback = build_measurement_stage_callback_for_stage_name(stage_name)
    synthetic_frame_pixel_array = numpy.random.default_rng(seed=1702).integers(
        0,
        256,
        size=(frame_height, frame_width, FRAME_CHANNEL_COUNT),
        dtype=numpy.uint8,
    )
    for _ in range(STAGE_CALLBACK_CALIBRATION_WARMUP_ITERATION_COUNT):
        measurement_stage_callback(synthetic_frame_pixel_array)

    observed_nanosecond_samples = []
    for _ in range(STAGE_CALLBACK_CALIBRATION_ITERATION_COUNT):
        started_at_nanoseconds = read_raw_monotonic_clock_nanoseconds()
        measurement_stage_callback(synthetic_frame_pixel_array)
        observed_nanosecond_samples.append(
            read_raw_monotonic_clock_nanoseconds() - started_at_nanoseconds
        )
    observed_nanosecond_samples.sort()
    return summarize_sorted_nanosecond_samples(observed_nanosecond_samples)


def build_runner_invocation_record(
    command_line_arguments,
    harness_arm,
    harness_command_line,
    runner_repetition_index,
    schedule_position_index,
    harness_repetition_index,
    cell_duration_seconds,
    warmup_exclusion_seconds,
    runner_interpreter_garbage_collection_configuration,
    stage_callback_cost_summary,
    measurement_cell_directory,
):
    """What this runner resolved for one cell, and where the harness's own
    authoritative specification for that cell lives.

    Deliberately carries no measurement parameter the harness records itself:
    two files claiming to be the cell's specification is how the artifact
    directory acquires two disagreeing answers."""
    return {
        "runner_mode": command_line_arguments.mode,
        "harness_arm": harness_arm,
        "stage_name": command_line_arguments.stage,
        "harness_stage_callback_module": HARNESS_STAGE_CALLBACK_MODULE_NAME,
        "harness_stage_callback_attribute": (
            resolve_harness_stage_callback_attribute_for_stage_name(
                command_line_arguments.stage
            )
        ),
        "harness_binary_path": os.path.abspath(command_line_arguments.harness_binary),
        "harness_command_line": harness_command_line,
        "harness_output_directory": os.path.abspath(command_line_arguments.output_dir),
        "measurement_cell_directory": measurement_cell_directory,
        "authoritative_cell_specification_path": os.path.join(
            measurement_cell_directory, HARNESS_CELL_SPECIFICATION_FILE_NAME
        ),
        "interleaved_schedule": list(
            build_interleaved_measurement_schedule_for_runner_mode(
                command_line_arguments.mode
            )
        ),
        "runner_repetition_index": runner_repetition_index,
        "schedule_position_index": schedule_position_index,
        "harness_repetition_index": harness_repetition_index,
        "cell_duration_seconds": cell_duration_seconds,
        "warmup_exclusion_seconds": warmup_exclusion_seconds,
        "expected_frame_count": command_line_arguments.fps * cell_duration_seconds,
        # Applied to the interpreter this runner runs in. Nothing configures the
        # harness's embedded interpreter, so this must never be read as the
        # cell's GC configuration.
        "runner_interpreter_garbage_collection_configuration": (
            runner_interpreter_garbage_collection_configuration
        ),
        "measured_stage_callback_cost_in_runner_interpreter": stage_callback_cost_summary,
        "python_version": sys.version,
        "numpy_version": numpy.__version__,
        "runner_module_path": os.path.abspath(__file__),
    }


def write_json_document(output_path, document):
    """Write one pretty-printed JSON document, newline-terminated."""
    with open(output_path, "w", encoding="utf-8") as json_output_file:
        json.dump(document, json_output_file, indent=2)
        json_output_file.write("\n")


def run_one_measurement_cell(
    command_line_arguments,
    cell_runner_mode,
    runner_repetition_index,
    schedule_position_index,
    schedule_length,
    cell_duration_seconds,
    warmup_exclusion_seconds,
    runner_interpreter_garbage_collection_configuration,
    stage_callback_cost_summary,
):
    """Run one cell to completion. Returns the harness's exit status."""
    harness_arm = resolve_harness_arm_for_runner_mode(cell_runner_mode)
    harness_repetition_index = build_harness_repetition_index(
        runner_repetition_index, schedule_position_index, schedule_length
    )
    harness_command_line = build_tier_a_harness_command_line(
        command_line_arguments,
        harness_arm,
        harness_repetition_index,
        cell_duration_seconds,
        warmup_exclusion_seconds,
        command_line_arguments.gc,
    )
    MEASUREMENT_CELL_LOGGER.info(
        "repetition %d position %d of %d: invoking harness: %s",
        runner_repetition_index,
        schedule_position_index,
        schedule_length,
        " ".join(harness_command_line),
    )

    garbage_collection_recorder = GarbageCollectionPauseAttributionRecorder()
    garbage_collection_recorder.install_collection_phase_callback()
    harness_started_at_epoch_seconds = time.time()
    try:
        completed_harness_process = subprocess.run(
            harness_command_line,
            env=build_tier_a_harness_process_environment(),
            check=False,
        )
    finally:
        garbage_collection_recorder.uninstall_collection_phase_callback()
    harness_exit_status = completed_harness_process.returncode

    measurement_cell_directory = locate_measurement_cell_directory_created_by_harness(
        os.path.abspath(command_line_arguments.output_dir),
        harness_started_at_epoch_seconds,
    )
    if measurement_cell_directory is None:
        MEASUREMENT_CELL_LOGGER.error(
            "harness exited %d and left no cell directory under %s",
            harness_exit_status,
            os.path.abspath(command_line_arguments.output_dir),
        )
        return harness_exit_status if harness_exit_status != 0 else 2
    MEASUREMENT_CELL_LOGGER.info(
        "harness exited %d for cell %s", harness_exit_status, measurement_cell_directory
    )

    written_record_count = garbage_collection_recorder.export_collection_records_as_jsonl(
        os.path.join(
            measurement_cell_directory,
            RUNNER_INTERPRETER_GARBAGE_COLLECTION_RECORD_FILE_NAME,
        )
    )
    MEASUREMENT_CELL_LOGGER.info(
        "recorded %d gc phase events in the runner interpreter while the cell ran",
        written_record_count,
    )

    write_json_document(
        os.path.join(measurement_cell_directory, RUNNER_INVOCATION_RECORD_FILE_NAME),
        build_runner_invocation_record(
            command_line_arguments,
            harness_arm,
            harness_command_line,
            runner_repetition_index,
            schedule_position_index,
            harness_repetition_index,
            cell_duration_seconds,
            warmup_exclusion_seconds,
            runner_interpreter_garbage_collection_configuration,
            stage_callback_cost_summary
            if cell_runner_mode == RUNNER_MODE_IN_PROCESS_PYTHON
            else None,
            measurement_cell_directory,
        ),
    )
    return harness_exit_status


def run_interleaved_measurement_schedule(command_line_arguments):
    """Run every cell of every repetition in A/B/A order, stopping at the first
    cell the harness refuses. Returns the process exit status."""
    resolve_harness_arm_for_runner_mode(command_line_arguments.mode)
    if command_line_arguments.mode == RUNNER_MODE_SUBPROCESS_PYTHON:
        require_subprocess_baseline_arm_is_provisioned()
    cell_duration_seconds = resolve_cell_duration_seconds(command_line_arguments.duration)
    warmup_exclusion_seconds = resolve_warmup_exclusion_seconds(
        command_line_arguments.warmup_exclusion_seconds, cell_duration_seconds
    )
    interleaved_schedule = build_interleaved_measurement_schedule_for_runner_mode(
        command_line_arguments.mode
    )

    if not command_line_arguments.harness_binary:
        MEASUREMENT_CELL_LOGGER.error(
            "no harness binary; pass --harness-binary or set $%s",
            HARNESS_BINARY_ENVIRONMENT_VARIABLE,
        )
        return 2
    if not os.path.isfile(command_line_arguments.harness_binary):
        MEASUREMENT_CELL_LOGGER.error(
            "harness binary %s does not exist; run `cargo build --release`",
            command_line_arguments.harness_binary,
        )
        return 2

    garbage_collection_recorder = GarbageCollectionPauseAttributionRecorder()
    runner_interpreter_garbage_collection_configuration = (
        garbage_collection_recorder.apply_garbage_collection_mode(
            command_line_arguments.gc
        )
    )
    MEASUREMENT_CELL_LOGGER.info(
        "gc mode %s applied to the runner interpreter only; the harness's embedded "
        "interpreter runs its own default configuration",
        command_line_arguments.gc,
    )

    stage_callback_cost_summary = measure_stage_callback_cost_in_runner_interpreter(
        command_line_arguments.stage,
        command_line_arguments.frame_width,
        command_line_arguments.frame_height,
    )
    MEASUREMENT_CELL_LOGGER.info(
        "stage %s measured cost p50=%.3fms p99=%.3fms max=%.3fms",
        command_line_arguments.stage,
        stage_callback_cost_summary["p50_nanoseconds"] / 1e6,
        stage_callback_cost_summary["p99_nanoseconds"] / 1e6,
        stage_callback_cost_summary["max_nanoseconds"] / 1e6,
    )

    os.makedirs(os.path.abspath(command_line_arguments.output_dir), exist_ok=True)

    if command_line_arguments.skip_harness_invocation:
        MEASUREMENT_CELL_LOGGER.info(
            "skipping harness invocation; schedule for %d repetition(s): %s",
            command_line_arguments.repetitions,
            " -> ".join(interleaved_schedule),
        )
        return 0

    for repetition_offset in range(command_line_arguments.repetitions):
        runner_repetition_index = (
            command_line_arguments.repetition_index + repetition_offset
        )
        for schedule_position_index, cell_runner_mode in enumerate(interleaved_schedule):
            harness_exit_status = run_one_measurement_cell(
                command_line_arguments,
                cell_runner_mode,
                runner_repetition_index,
                schedule_position_index,
                len(interleaved_schedule),
                cell_duration_seconds,
                warmup_exclusion_seconds,
                runner_interpreter_garbage_collection_configuration,
                stage_callback_cost_summary,
            )
            if harness_exit_status != 0:
                MEASUREMENT_CELL_LOGGER.error(
                    "stopping the schedule: repetition %d position %d exited %d",
                    runner_repetition_index,
                    schedule_position_index,
                    harness_exit_status,
                )
                return harness_exit_status
    return 0


def parse_measurement_cell_command_line_arguments(command_line_argument_values=None):
    """Parse the schedule's parameters."""
    argument_parser = argparse.ArgumentParser(
        prog="runner.py",
        description="Run an A/B/A interleaved schedule of #1702 measurement cells.",
    )
    argument_parser.add_argument("--fps", type=int, required=True)
    argument_parser.add_argument("--stage", choices=SUPPORTED_STAGE_NAMES, required=True)
    argument_parser.add_argument(
        "--mode",
        choices=SUPPORTED_RUNNER_MODES,
        required=True,
        help="the arm under test; rust-floor is a pure-Rust passthrough that "
        "isolates engine wire-hop cost from PyO3 cost, and is also the A of the "
        "A/B/A schedule. subprocess is today's model and requires "
        "provision_subprocess_baseline_package.py to have run",
    )
    argument_parser.add_argument(
        "--gc",
        choices=(GARBAGE_COLLECTION_MODE_DEFAULT, GARBAGE_COLLECTION_MODE_TUNED),
        default=GARBAGE_COLLECTION_MODE_DEFAULT,
        help="GC configuration for the runner's own interpreter; it does not "
        "reach the harness's embedded interpreter",
    )
    argument_parser.add_argument(
        "--duration", type=float, required=True, help="per-cell duration in whole seconds"
    )
    argument_parser.add_argument("--output-dir", required=True)
    argument_parser.add_argument("--frame-width", type=int, default=1920)
    argument_parser.add_argument("--frame-height", type=int, default=1080)
    argument_parser.add_argument(
        "--warmup-exclusion-seconds",
        type=int,
        default=None,
        help=f"defaults to {PROTOCOL_WARMUP_EXCLUSION_SECONDS}s for cells long "
        f"enough to carry it, {EXPLORATORY_CELL_WARMUP_EXCLUSION_SECONDS}s otherwise",
    )
    argument_parser.add_argument(
        "--repetition-index",
        type=int,
        default=0,
        help="index of the first repetition, so a second schedule can extend an "
        "existing output directory without overwriting its cells",
    )
    argument_parser.add_argument(
        "--repetitions",
        type=int,
        default=1,
        help="how many A/B/A repetitions to run; each contributes one harness "
        "process per cell",
    )
    argument_parser.add_argument("--disable-gil-anchor", action="store_true")
    argument_parser.add_argument("--require-locked-measurement-state", action="store_true")
    argument_parser.add_argument(
        "--harness-binary",
        default=os.environ.get(HARNESS_BINARY_ENVIRONMENT_VARIABLE),
        help="path to the built Rust harness binary; defaults to "
        f"${HARNESS_BINARY_ENVIRONMENT_VARIABLE}",
    )
    argument_parser.add_argument(
        "--skip-harness-invocation",
        action="store_true",
        help="resolve and log the schedule without running any cell",
    )
    return argument_parser.parse_args(command_line_argument_values)


def configure_measurement_cell_logging():
    """Route runner output through `logging`; bare print would interleave badly
    with the harness's own stdout in an A/B/A schedule."""
    logging.basicConfig(
        level=logging.INFO,
        stream=sys.stderr,
        format="%(asctime)s %(levelname)s %(name)s %(message)s",
    )


def main():
    """Entry point: exit status is the first failing cell's, or 0."""
    configure_measurement_cell_logging()
    command_line_arguments = parse_measurement_cell_command_line_arguments()
    try:
        return run_interleaved_measurement_schedule(command_line_arguments)
    except (SubprocessBaselineArmIsNotProvisionedError, ValueError) as refusal:
        MEASUREMENT_CELL_LOGGER.error("%s", refusal)
        return 2


if __name__ == "__main__":
    sys.exit(main())
