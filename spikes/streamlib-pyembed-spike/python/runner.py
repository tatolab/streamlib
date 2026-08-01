#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Measurement-cell driver for the #1702 in-process-Python spike: resolves one
cell of the (arm x fps x stage x gc) matrix, records its spec, and runs it.

Throwaway spike code. Its API shape is explicitly not a proposal for the SDK."""

import argparse
import json
import logging
import os
import subprocess
import sys

import numpy

from gc_collection_attribution import (
    GARBAGE_COLLECTION_MODE_DEFAULT,
    GARBAGE_COLLECTION_MODE_TUNED,
    GarbageCollectionPauseAttributionRecorder,
    read_raw_monotonic_clock_nanoseconds,
)

MEASUREMENT_CELL_LOGGER = logging.getLogger("streamlib_pyembed_spike.runner")

MEASUREMENT_ARM_IN_PROCESS_PYTHON = "in-process"
MEASUREMENT_ARM_SUBPROCESS_PYTHON = "subprocess"
MEASUREMENT_ARM_RUST_PASSTHROUGH_FLOOR = "rust-floor"
SUPPORTED_MEASUREMENT_ARMS = (
    MEASUREMENT_ARM_IN_PROCESS_PYTHON,
    MEASUREMENT_ARM_SUBPROCESS_PYTHON,
    MEASUREMENT_ARM_RUST_PASSTHROUGH_FLOOR,
)

STAGE_NAME_PASSTHROUGH = "passthrough"
STAGE_NAME_REALISTIC = "realistic"
SUPPORTED_STAGE_NAMES = (STAGE_NAME_PASSTHROUGH, STAGE_NAME_REALISTIC)

# Owner decision for #1702: latency percentiles are the primary signal and every
# emitted sample must reach the sink, so no lossy delivery profile is offered.
# Drops are counted and reported but do not gate.
RESOLVED_DELIVERY_PROFILE = "every_sample"

# The metric the Rust sink computes from the in-band preamble. "capture to
# present" is NOT observable in this rig and must never be used as a metric name.
PRIMARY_LATENCY_METRIC_NAME = "source_emit_to_sink_receive"

FRAME_CHANNEL_COUNT = 4
REALISTIC_STAGE_CALIBRATION_ITERATION_COUNT = 200
REALISTIC_STAGE_CALIBRATION_WARMUP_ITERATION_COUNT = 10

# Tuned on this rig (Ryzen-class x86_64, CPython 3.12.3, numpy 2.4.4) so the
# realistic stage lands inside the ticket's 2-5ms budget for 1920x1080x4 uint8:
# measured p50 ~2.5ms, p99 ~4.5ms. Changing any of these three constants
# re-tunes the cost and invalidates cross-cell comparison.
REALISTIC_STAGE_GAIN_NUMERATOR = 5
REALISTIC_STAGE_GAIN_DENOMINATOR = 4
REALISTIC_STAGE_BRIGHTNESS_BIAS = 8

# PROVISIONAL, see the contract note on
# `invoke_rust_measurement_harness_for_measurement_cell`: the handover mechanism
# for the in-process arm's stage callable is not finalized.
STAGE_CALLBACK_MODULE_ENVIRONMENT_VARIABLE = (
    "STREAMLIB_PYEMBED_SPIKE_STAGE_CALLBACK_MODULE_PATH"
)
STAGE_CALLBACK_FACTORY_ENVIRONMENT_VARIABLE = (
    "STREAMLIB_PYEMBED_SPIKE_STAGE_CALLBACK_FACTORY_NAME"
)
HARNESS_BINARY_ENVIRONMENT_VARIABLE = "STREAMLIB_PYEMBED_SPIKE_HARNESS_BINARY"

MEASUREMENT_CELL_SPECIFICATION_FILE_NAME = "cell-spec.json"
# Two files, because under the exec arrangement two interpreters exist and only
# the one running the stage callback can explain a frame-latency tail spike.
RUNNER_INTERPRETER_GARBAGE_COLLECTION_RECORD_FILE_NAME = (
    "gc-collections-runner-interpreter.jsonl"
)
EMBEDDED_INTERPRETER_GARBAGE_COLLECTION_RECORD_FILE_NAME = (
    "gc-collections-embedded-interpreter.jsonl"
)


def passthrough_measurement_stage_callback(frame_pixel_array):
    """The zero-work stage: everything it costs is GIL acquisition plus building
    the numpy view over the Rust buffer."""
    return None


def build_realistic_measurement_stage_callback(frame_width, frame_height):
    """Build the ~2-5ms numpy stage callback for a frame geometry.

    The returned callable mutates the caller's array in place. It never rebinds
    the name: the array aliases a Rust-owned buffer, so a rebind would discard
    the work silently."""
    expected_element_count = frame_width * frame_height * FRAME_CHANNEL_COUNT
    expected_frame_shape = (frame_height, frame_width, FRAME_CHANNEL_COUNT)
    # Sized once and reused. A per-frame 16 MB int16 temporary would make this
    # cell an allocator benchmark: the in-process arm shares the engine's
    # allocator and the subprocess arm does not, so that churn would land
    # unevenly on the two arms' tails and confound the comparison being made.
    intermediate_pixel_scratch = numpy.empty(expected_frame_shape, dtype=numpy.int16)

    def realistic_measurement_stage_callback(frame_pixel_array):
        frame_pixel_view = _reshape_frame_pixel_array_without_copying(
            frame_pixel_array, expected_element_count, expected_frame_shape
        )
        numpy.copyto(intermediate_pixel_scratch, frame_pixel_view, casting="unsafe")
        numpy.multiply(
            intermediate_pixel_scratch,
            REALISTIC_STAGE_GAIN_NUMERATOR,
            out=intermediate_pixel_scratch,
        )
        numpy.floor_divide(
            intermediate_pixel_scratch,
            REALISTIC_STAGE_GAIN_DENOMINATOR,
            out=intermediate_pixel_scratch,
        )
        numpy.add(
            intermediate_pixel_scratch,
            REALISTIC_STAGE_BRIGHTNESS_BIAS,
            out=intermediate_pixel_scratch,
        )
        numpy.clip(intermediate_pixel_scratch, 0, 255, out=intermediate_pixel_scratch)
        numpy.copyto(frame_pixel_view, intermediate_pixel_scratch, casting="unsafe")
        return None

    return realistic_measurement_stage_callback


def _reshape_frame_pixel_array_without_copying(
    frame_pixel_array, expected_element_count, expected_frame_shape
):
    if frame_pixel_array.size != expected_element_count:
        raise ValueError(
            f"frame carries {frame_pixel_array.size} elements, "
            f"cell geometry expects {expected_element_count}"
        )
    if frame_pixel_array.shape == expected_frame_shape:
        return frame_pixel_array
    # numpy.reshape falls back to a copy when the source is not C-contiguous,
    # and a copy would send every pixel write to a temporary the Rust side never
    # reads. Refuse rather than silently produce a no-op stage.
    if not frame_pixel_array.flags.c_contiguous:
        raise ValueError(
            "frame buffer is not C-contiguous; reshaping it would copy and the "
            "stage would no longer write through to the Rust buffer"
        )
    return frame_pixel_array.reshape(expected_frame_shape)


def build_measurement_stage_callback_for_stage_name(
    stage_name, frame_width, frame_height
):
    """Resolve a `--stage` value to the callable the harness invokes per frame."""
    if stage_name == STAGE_NAME_PASSTHROUGH:
        return passthrough_measurement_stage_callback
    if stage_name == STAGE_NAME_REALISTIC:
        return build_realistic_measurement_stage_callback(frame_width, frame_height)
    raise ValueError(
        f"unsupported stage name {stage_name!r}; expected one of {SUPPORTED_STAGE_NAMES}"
    )


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


def measure_stage_callback_cost_on_this_machine(
    measurement_stage_callback, frame_width, frame_height
):
    """Time the stage callback against a synthetic frame so the cell spec records
    what the stage actually costs on the machine the cell ran on."""
    synthetic_frame_pixel_array = numpy.random.default_rng(seed=1702).integers(
        0,
        256,
        size=(frame_height, frame_width, FRAME_CHANNEL_COUNT),
        dtype=numpy.uint8,
    )
    for _ in range(REALISTIC_STAGE_CALIBRATION_WARMUP_ITERATION_COUNT):
        measurement_stage_callback(synthetic_frame_pixel_array)

    observed_nanosecond_samples = []
    for _ in range(REALISTIC_STAGE_CALIBRATION_ITERATION_COUNT):
        started_at_nanoseconds = read_raw_monotonic_clock_nanoseconds()
        measurement_stage_callback(synthetic_frame_pixel_array)
        observed_nanosecond_samples.append(
            read_raw_monotonic_clock_nanoseconds() - started_at_nanoseconds
        )
    observed_nanosecond_samples.sort()
    return summarize_sorted_nanosecond_samples(observed_nanosecond_samples)


def build_measurement_cell_directory_name(
    measurement_arm, frames_per_second, stage_name, garbage_collection_mode,
    repetition_index,
):
    """Unambiguous, lexically sortable directory name for one cell.

    Zero-padded so a plain `sort` orders 8fps before 60fps, and arm-first so an
    A/B/A schedule's cells still group by arm on disk."""
    return (
        f"arm-{measurement_arm}"
        f"__fps-{frames_per_second:03d}"
        f"__stage-{stage_name}"
        f"__gc-{garbage_collection_mode}"
        f"__rep-{repetition_index:03d}"
    )


def build_measurement_cell_specification(
    command_line_arguments,
    garbage_collection_configuration,
    stage_callback_cost_summary,
    measurement_cell_directory,
):
    """Every resolved parameter of the cell.

    The summarizer refuses to evaluate a gate the recorded configuration cannot
    support, so a missing field here silently drops a gate from the artifact."""
    return {
        "measurement_arm": command_line_arguments.mode,
        "frames_per_second": command_line_arguments.fps,
        "stage_name": command_line_arguments.stage,
        "garbage_collection_mode": command_line_arguments.gc,
        "duration_seconds": command_line_arguments.duration,
        "frame_width": command_line_arguments.frame_width,
        "frame_height": command_line_arguments.frame_height,
        "frame_channel_count": FRAME_CHANNEL_COUNT,
        "repetition_index": command_line_arguments.repetition_index,
        "output_directory": os.path.abspath(command_line_arguments.output_dir),
        "measurement_cell_directory": os.path.abspath(measurement_cell_directory),
        "delivery_profile": RESOLVED_DELIVERY_PROFILE,
        "primary_latency_metric_name": PRIMARY_LATENCY_METRIC_NAME,
        "expected_frame_count": int(
            command_line_arguments.fps * command_line_arguments.duration
        ),
        "applied_garbage_collection_configuration": garbage_collection_configuration,
        "measured_stage_callback_cost": stage_callback_cost_summary,
        "realistic_stage_operation": (
            "int16 scratch: gain "
            f"{REALISTIC_STAGE_GAIN_NUMERATOR}/{REALISTIC_STAGE_GAIN_DENOMINATOR}, "
            f"bias +{REALISTIC_STAGE_BRIGHTNESS_BIAS}, clip 0..255, "
            "written back in place"
        ),
        "python_version": sys.version,
        "numpy_version": numpy.__version__,
        "runner_module_path": os.path.abspath(__file__),
    }


def invoke_rust_measurement_harness_for_measurement_cell(
    harness_binary_path, measurement_cell_directory, measurement_cell_specification_path
):
    """Run the Rust side of the cell. This is the only place the two languages meet.

    Contract as implemented (subprocess exec):
      argv: <harness_binary_path>
            --cell-directory <measurement_cell_directory>
            --cell-specification <measurement_cell_specification_path>
      The harness reads every resolved parameter from the cell spec, writes its
      per-frame JSONL into the cell directory, and exits 0 only on a clean run.
      For the `in-process` arm it loads this module and calls
      `build_measurement_stage_callback_for_stage_name`, located via the two
      STAGE_CALLBACK_* environment variables set below.

    OPEN: the orchestrator has not settled whether the harness is a binary that
    embeds CPython (this path) or a cdylib that CPython imports (in which case
    this function becomes an import plus a call, and the environment variables
    below are unnecessary). Both arrangements keep the boundary in this one
    function."""
    harness_environment = dict(os.environ)
    harness_environment[STAGE_CALLBACK_MODULE_ENVIRONMENT_VARIABLE] = os.path.abspath(
        __file__
    )
    harness_environment[STAGE_CALLBACK_FACTORY_ENVIRONMENT_VARIABLE] = (
        "build_measurement_stage_callback_for_stage_name"
    )
    harness_command_line = [
        harness_binary_path,
        "--cell-directory",
        measurement_cell_directory,
        "--cell-specification",
        measurement_cell_specification_path,
    ]
    MEASUREMENT_CELL_LOGGER.info(
        "invoking rust measurement harness: %s", " ".join(harness_command_line)
    )
    completed_harness_process = subprocess.run(
        harness_command_line, env=harness_environment, check=False
    )
    return completed_harness_process.returncode


def set_up_embedded_interpreter_for_measurement_cell(measurement_cell_specification):
    """Call this from inside the interpreter that will actually run the stage
    callback, before the first frame.

    Under the exec arrangement that interpreter is the one the harness binary
    embeds, not the one running this module's `__main__` — a GC recorder
    installed in the wrong interpreter records collections that cannot have
    caused any frame's latency. Returns the stage callback paired with the
    recorder to hand back to `tear_down_embedded_interpreter_for_measurement_cell`."""
    garbage_collection_recorder = GarbageCollectionPauseAttributionRecorder()
    garbage_collection_recorder.apply_garbage_collection_mode(
        measurement_cell_specification["garbage_collection_mode"]
    )
    garbage_collection_recorder.install_collection_phase_callback()
    measurement_stage_callback = build_measurement_stage_callback_for_stage_name(
        measurement_cell_specification["stage_name"],
        measurement_cell_specification["frame_width"],
        measurement_cell_specification["frame_height"],
    )
    return measurement_stage_callback, garbage_collection_recorder


def tear_down_embedded_interpreter_for_measurement_cell(
    garbage_collection_recorder, measurement_cell_specification
):
    """Stop recording and flush the embedded interpreter's GC records into the
    cell directory. Returns the number of phase events written."""
    garbage_collection_recorder.uninstall_collection_phase_callback()
    return garbage_collection_recorder.export_collection_records_as_jsonl(
        os.path.join(
            measurement_cell_specification["measurement_cell_directory"],
            EMBEDDED_INTERPRETER_GARBAGE_COLLECTION_RECORD_FILE_NAME,
        )
    )


def parse_measurement_cell_command_line_arguments(command_line_argument_values=None):
    """Parse one cell's parameters."""
    argument_parser = argparse.ArgumentParser(
        prog="runner.py", description="Run one #1702 measurement cell."
    )
    argument_parser.add_argument("--fps", type=int, required=True)
    argument_parser.add_argument(
        "--stage", choices=SUPPORTED_STAGE_NAMES, required=True
    )
    argument_parser.add_argument(
        "--mode",
        choices=SUPPORTED_MEASUREMENT_ARMS,
        required=True,
        help="the comparison arm; rust-floor is a pure-Rust passthrough that "
        "isolates engine wire-hop cost from PyO3 cost",
    )
    argument_parser.add_argument(
        "--gc",
        choices=(GARBAGE_COLLECTION_MODE_DEFAULT, GARBAGE_COLLECTION_MODE_TUNED),
        default=GARBAGE_COLLECTION_MODE_DEFAULT,
    )
    argument_parser.add_argument("--duration", type=float, required=True)
    argument_parser.add_argument("--output-dir", required=True)
    argument_parser.add_argument("--frame-width", type=int, default=1920)
    argument_parser.add_argument("--frame-height", type=int, default=1080)
    argument_parser.add_argument("--repetition-index", type=int, default=0)
    argument_parser.add_argument(
        "--harness-binary",
        default=os.environ.get(HARNESS_BINARY_ENVIRONMENT_VARIABLE),
        help="path to the built Rust harness binary; defaults to "
        f"${HARNESS_BINARY_ENVIRONMENT_VARIABLE}",
    )
    argument_parser.add_argument(
        "--skip-harness-invocation",
        action="store_true",
        help="resolve the cell and write cell-spec.json without running it",
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


def run_measurement_cell(command_line_arguments):
    """Resolve, record and run one cell. Returns the process exit status."""
    measurement_cell_directory = os.path.join(
        os.path.abspath(command_line_arguments.output_dir),
        build_measurement_cell_directory_name(
            command_line_arguments.mode,
            command_line_arguments.fps,
            command_line_arguments.stage,
            command_line_arguments.gc,
            command_line_arguments.repetition_index,
        ),
    )
    os.makedirs(measurement_cell_directory, exist_ok=True)

    garbage_collection_recorder = GarbageCollectionPauseAttributionRecorder()
    garbage_collection_configuration = (
        garbage_collection_recorder.apply_garbage_collection_mode(
            command_line_arguments.gc
        )
    )

    measurement_stage_callback = build_measurement_stage_callback_for_stage_name(
        command_line_arguments.stage,
        command_line_arguments.frame_width,
        command_line_arguments.frame_height,
    )
    stage_callback_cost_summary = measure_stage_callback_cost_on_this_machine(
        measurement_stage_callback,
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

    measurement_cell_specification = build_measurement_cell_specification(
        command_line_arguments,
        garbage_collection_configuration,
        stage_callback_cost_summary,
        measurement_cell_directory,
    )
    measurement_cell_specification_path = os.path.join(
        measurement_cell_directory, MEASUREMENT_CELL_SPECIFICATION_FILE_NAME
    )
    with open(
        measurement_cell_specification_path, "w", encoding="utf-8"
    ) as specification_file:
        json.dump(measurement_cell_specification, specification_file, indent=2)
        specification_file.write("\n")
    MEASUREMENT_CELL_LOGGER.info(
        "wrote cell spec %s", measurement_cell_specification_path
    )

    if command_line_arguments.skip_harness_invocation:
        return 0
    if not command_line_arguments.harness_binary:
        MEASUREMENT_CELL_LOGGER.error(
            "no harness binary; pass --harness-binary or set $%s",
            HARNESS_BINARY_ENVIRONMENT_VARIABLE,
        )
        return 2

    garbage_collection_recorder.install_collection_phase_callback()
    try:
        harness_exit_status = invoke_rust_measurement_harness_for_measurement_cell(
            command_line_arguments.harness_binary,
            measurement_cell_directory,
            measurement_cell_specification_path,
        )
    finally:
        garbage_collection_recorder.uninstall_collection_phase_callback()
        written_record_count = (
            garbage_collection_recorder.export_collection_records_as_jsonl(
                os.path.join(
                    measurement_cell_directory,
                    RUNNER_INTERPRETER_GARBAGE_COLLECTION_RECORD_FILE_NAME,
                )
            )
        )
        MEASUREMENT_CELL_LOGGER.info(
            "recorded %d gc phase events in the runner interpreter", written_record_count
        )

    if harness_exit_status != 0:
        MEASUREMENT_CELL_LOGGER.error(
            "harness exited %d for cell %s",
            harness_exit_status,
            measurement_cell_directory,
        )
    return harness_exit_status


if __name__ == "__main__":
    configure_measurement_cell_logging()
    sys.exit(run_measurement_cell(parse_measurement_cell_command_line_arguments()))
