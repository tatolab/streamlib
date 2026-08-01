# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Contract tests for the Python side of the spike.

The load-bearing one is `the_command_line_runner_py_builds_is_accepted_by_the_harness`:
runner.py and `src/bin/tier_a_harness.rs` are written independently and nothing
else proves their argv agree. Runnable with
`python3 -m unittest discover -s python`; no pytest dependency."""

import gc
import os
import subprocess
import tempfile
import time
import unittest
import unittest.mock

import numpy

import runner
import spike_stage_callbacks
from gc_collection_attribution import GarbageCollectionPauseAttributionRecorder

SPIKE_CRATE_DIRECTORY = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEFAULT_HARNESS_BINARY_PATH = os.path.join(
    SPIKE_CRATE_DIRECTORY, "target", "release", "tier_a_harness"
)

# Long enough for the source to emit past the exploratory warmup exclusion, short
# enough that the suite stays a smoke test.
CONTRACT_CELL_DURATION_SECONDS = "3"
# The contract under test is argv acceptance, not throughput, so the smallest
# geometry that still exercises the whole graph is the right one.
CONTRACT_FRAME_WIDTH_PIXELS = "320"
CONTRACT_FRAME_HEIGHT_PIXELS = "240"

HARNESS_PROCESS_TIMEOUT_SECONDS = 180


def locate_built_harness_binary_path():
    """The release binary, or None when the checkout has not been built."""
    harness_binary_path = os.environ.get(
        runner.HARNESS_BINARY_ENVIRONMENT_VARIABLE, DEFAULT_HARNESS_BINARY_PATH
    )
    return harness_binary_path if os.path.isfile(harness_binary_path) else None


def require_built_harness_binary_path():
    """Skip rather than fail on a clean checkout: an unbuilt binary is a missing
    prerequisite, not a broken contract."""
    harness_binary_path = locate_built_harness_binary_path()
    if harness_binary_path is None:
        raise unittest.SkipTest(
            f"no harness binary at {DEFAULT_HARNESS_BINARY_PATH}; run "
            "`cargo build --release` in spikes/streamlib-pyembed-spike"
        )
    return harness_binary_path


class TierAHarnessCommandLineContractTest(unittest.TestCase):
    """runner.py's argv against the harness that has to accept it."""

    def build_contract_command_line_arguments(self, harness_binary_path, output_directory):
        return runner.parse_measurement_cell_command_line_arguments(
            [
                "--fps",
                "30",
                "--stage",
                "passthrough",
                "--mode",
                runner.RUNNER_MODE_IN_PROCESS_PYTHON,
                "--duration",
                CONTRACT_CELL_DURATION_SECONDS,
                "--frame-width",
                CONTRACT_FRAME_WIDTH_PIXELS,
                "--frame-height",
                CONTRACT_FRAME_HEIGHT_PIXELS,
                "--output-dir",
                output_directory,
                "--harness-binary",
                harness_binary_path,
            ]
        )

    def test_every_flag_runner_py_emits_is_declared_by_the_harness(self):
        """Fails fast and names the offending flag; the full run below is the
        stronger but slower form of the same assertion."""
        harness_binary_path = require_built_harness_binary_path()
        harness_help_text = subprocess.run(
            [harness_binary_path, "--help"],
            check=True,
            capture_output=True,
            text=True,
            timeout=HARNESS_PROCESS_TIMEOUT_SECONDS,
        ).stdout

        with tempfile.TemporaryDirectory() as output_directory:
            harness_command_line = runner.build_tier_a_harness_command_line(
                self.build_contract_command_line_arguments(
                    harness_binary_path, output_directory
                ),
                runner.HARNESS_ARM_BY_RUNNER_MODE[runner.RUNNER_MODE_IN_PROCESS_PYTHON],
                0,
                3,
                1,
                runner.GARBAGE_COLLECTION_MODE_DEFAULT,
            )
        emitted_flags = [
            argument for argument in harness_command_line if argument.startswith("--")
        ]
        self.assertTrue(emitted_flags)
        for emitted_flag in emitted_flags:
            self.assertIn(
                emitted_flag,
                harness_help_text,
                f"runner.py emits {emitted_flag}, which the harness does not declare",
            )

    def test_the_command_line_runner_py_builds_is_accepted_by_the_harness(self):
        harness_binary_path = require_built_harness_binary_path()
        with tempfile.TemporaryDirectory() as output_directory:
            command_line_arguments = self.build_contract_command_line_arguments(
                harness_binary_path, output_directory
            )
            harness_command_line = runner.build_tier_a_harness_command_line(
                command_line_arguments,
                runner.HARNESS_ARM_BY_RUNNER_MODE[runner.RUNNER_MODE_IN_PROCESS_PYTHON],
                0,
                int(CONTRACT_CELL_DURATION_SECONDS),
                1,
                runner.GARBAGE_COLLECTION_MODE_DEFAULT,
            )

            harness_started_at_epoch_seconds = time.time()
            completed_harness_process = subprocess.run(
                harness_command_line,
                env=runner.build_tier_a_harness_process_environment(),
                check=False,
                capture_output=True,
                text=True,
                timeout=HARNESS_PROCESS_TIMEOUT_SECONDS,
            )
            self.assertEqual(
                completed_harness_process.returncode,
                0,
                "the harness rejected runner.py's command line:\n"
                f"{' '.join(harness_command_line)}\n"
                f"{completed_harness_process.stderr}",
            )

            measurement_cell_directory = (
                runner.locate_measurement_cell_directory_created_by_harness(
                    output_directory, harness_started_at_epoch_seconds
                )
            )
            self.assertIsNotNone(
                measurement_cell_directory,
                f"the harness left no cell directory under {output_directory}: "
                f"{os.listdir(output_directory)}",
            )
            self.assertTrue(
                os.path.isfile(
                    os.path.join(
                        measurement_cell_directory, runner.HARNESS_CELL_SUMMARY_FILE_NAME
                    )
                )
            )

    def test_subprocess_mode_resolves_to_the_baseline_arm(self):
        self.assertEqual(
            runner.resolve_harness_arm_for_runner_mode(
                runner.RUNNER_MODE_SUBPROCESS_PYTHON
            ),
            "subprocess-python-baseline",
        )

    def test_every_runner_mode_maps_to_a_harness_arm(self):
        """A mode accepted by argparse but absent from the table would fail
        mid-schedule, after earlier cells had already burned their minutes."""
        for runner_mode in runner.SUPPORTED_RUNNER_MODES:
            self.assertIn(runner_mode, runner.HARNESS_ARM_BY_RUNNER_MODE)

    def test_an_unprovisioned_subprocess_arm_is_refused_before_any_cell_runs(self):
        """The failure this replaces is silent: an unprovisioned slot fails at
        graph build, and an unattended matrix would record the baseline arm as
        absent rather than as unrunnable."""
        with unittest.mock.patch("os.path.isfile", return_value=False):
            with self.assertRaises(
                runner.SubprocessBaselineArmIsNotProvisionedError
            ) as refusal:
                runner.require_subprocess_baseline_arm_is_provisioned()
        self.assertIn("provision_subprocess_baseline_package.py", str(refusal.exception))

    def test_the_harness_environment_carries_both_subprocess_arm_pins(self):
        """Both are load-bearing and both fail silently when absent.

        Losing the cdylib pin is the one that actually happened: the spawned
        subprocess resolved a different `libstreamlib_python_native.so`, failed
        to open its own input channel with
        `DoesNotSupportRequestedAmountOfPublishers`, and the cell reported an
        arm that produced no frames rather than one that was misconfigured. It
        only reproduced under the runner, because a hand-run cell inherits the
        variable from the operator's shell."""
        if not os.path.isfile(
            os.path.join(
                runner.SPIKE_CRATE_ROOT_DIRECTORY,
                ".provisioned",
                "provisioning-record.json",
            )
        ):
            raise unittest.SkipTest(
                "no provisioning record; run "
                "python/provision_subprocess_baseline_package.py"
            )
        environment = runner.build_tier_a_harness_process_environment()
        self.assertTrue(
            os.path.isdir(
                environment[runner.APP_MODULES_DIRECTORY_ENVIRONMENT_VARIABLE]
            )
        )
        self.assertTrue(
            os.path.isfile(
                environment[runner.PYTHON_NATIVE_LIBRARY_ENVIRONMENT_VARIABLE]
            )
        )

    def test_an_unprovisioned_checkout_yields_no_subprocess_arm_environment(self):
        """The refusal belongs to the provisioning guard, which names the fix.
        Silently exporting a guessed path here would restore the failure above."""
        with unittest.mock.patch("os.path.isfile", return_value=False):
            self.assertEqual(runner.read_subprocess_baseline_arm_environment(), {})

    def test_a_fractional_duration_is_refused_rather_than_rounded(self):
        with self.assertRaises(ValueError):
            runner.resolve_cell_duration_seconds(2.5)

    def test_a_warmup_exclusion_covering_the_whole_cell_is_refused(self):
        """Such a cell exits 0 with all-zero percentiles, which reads like a very
        fast cell rather than an empty one."""
        with self.assertRaises(ValueError):
            runner.resolve_warmup_exclusion_seconds(5, 5)

    def test_a_short_cell_falls_back_to_the_exploratory_warmup_exclusion(self):
        self.assertEqual(
            runner.resolve_warmup_exclusion_seconds(None, 10),
            runner.EXPLORATORY_CELL_WARMUP_EXCLUSION_SECONDS,
        )
        self.assertEqual(
            runner.resolve_warmup_exclusion_seconds(
                None, runner.PROTOCOL_WARMUP_EXCLUSION_SECONDS * 10
            ),
            runner.PROTOCOL_WARMUP_EXCLUSION_SECONDS,
        )


class InterleavedMeasurementScheduleTest(unittest.TestCase):
    """The A/B/A ordering and the per-cell repetition indices it depends on."""

    def test_the_schedule_places_a_floor_cell_on_either_side_of_the_arm_under_test(self):
        self.assertEqual(
            runner.build_interleaved_measurement_schedule_for_runner_mode(
                runner.RUNNER_MODE_IN_PROCESS_PYTHON
            ),
            (
                runner.RUNNER_MODE_RUST_PASSTHROUGH_FLOOR,
                runner.RUNNER_MODE_IN_PROCESS_PYTHON,
                runner.RUNNER_MODE_RUST_PASSTHROUGH_FLOOR,
            ),
        )

    def test_the_floor_mode_runs_a_single_cell_per_repetition(self):
        self.assertEqual(
            runner.build_interleaved_measurement_schedule_for_runner_mode(
                runner.RUNNER_MODE_RUST_PASSTHROUGH_FLOOR
            ),
            (runner.RUNNER_MODE_RUST_PASSTHROUGH_FLOOR,),
        )

    def test_every_cell_of_every_repetition_gets_its_own_harness_repetition_index(self):
        """The harness names its cell directory from arm, rate, GIL anchor and
        repetition index — the two floor cells of one repetition agree on all but
        the last, so a shared index would overwrite a cell's raw data."""
        schedule = runner.build_interleaved_measurement_schedule_for_runner_mode(
            runner.RUNNER_MODE_IN_PROCESS_PYTHON
        )
        assigned_indices_by_arm = {}
        for runner_repetition_index in range(3):
            for schedule_position_index, cell_runner_mode in enumerate(schedule):
                harness_repetition_index = runner.build_harness_repetition_index(
                    runner_repetition_index, schedule_position_index, len(schedule)
                )
                assigned_indices_by_arm.setdefault(cell_runner_mode, []).append(
                    harness_repetition_index
                )
        for cell_runner_mode, assigned_indices in assigned_indices_by_arm.items():
            self.assertEqual(
                len(assigned_indices),
                len(set(assigned_indices)),
                f"{cell_runner_mode} cells collide on harness repetition indices "
                f"{assigned_indices}",
            )


class RealisticStageCallbackTest(unittest.TestCase):
    """The stage callback the harness calls per frame, against the aliasing rules
    the Rust side cannot enforce."""

    def test_realistic_stage_writes_through_a_view_into_the_original_buffer(self):
        """A rebind instead of an in-place write would leave the Rust-owned
        buffer untouched and turn the stage into a silent no-op."""
        original_frame_pixel_buffer = numpy.full((2, 3, 4), 100, dtype=numpy.uint8)
        frame_pixel_view = original_frame_pixel_buffer.view()
        self.assertTrue(frame_pixel_view.base is original_frame_pixel_buffer)

        spike_stage_callbacks.realistic_stage(frame_pixel_view)

        expected_pixel_value = (
            100
            * spike_stage_callbacks.REALISTIC_STAGE_GAIN_NUMERATOR
            // spike_stage_callbacks.REALISTIC_STAGE_GAIN_DENOMINATOR
            + spike_stage_callbacks.REALISTIC_STAGE_BRIGHTNESS_BIAS
        )
        self.assertTrue(
            numpy.array_equal(
                original_frame_pixel_buffer,
                numpy.full((2, 3, 4), expected_pixel_value, dtype=numpy.uint8),
            ),
            f"the original buffer still reads {original_frame_pixel_buffer.ravel()[:8]}",
        )

    def test_realistic_stage_refuses_a_non_c_contiguous_frame(self):
        """Reshaping a strided array copies, and every pixel write would land in
        the copy the Rust side never reads."""
        contiguous_pixel_buffer = numpy.zeros(64, dtype=numpy.uint8)
        strided_frame_pixel_view = contiguous_pixel_buffer[::2]
        self.assertFalse(strided_frame_pixel_view.flags.c_contiguous)

        with self.assertRaises(ValueError) as refusal:
            spike_stage_callbacks.realistic_stage(strided_frame_pixel_view)
        self.assertIn("C-contiguous", str(refusal.exception))

    def test_the_stage_name_the_runner_emits_resolves_to_a_real_callable(self):
        for stage_name in runner.SUPPORTED_STAGE_NAMES:
            callback_attribute_name = (
                runner.resolve_harness_stage_callback_attribute_for_stage_name(stage_name)
            )
            self.assertTrue(
                callable(getattr(spike_stage_callbacks, callback_attribute_name)),
                f"{runner.HARNESS_STAGE_CALLBACK_MODULE_NAME} exposes no callable "
                f"{callback_attribute_name}",
            )


class GarbageCollectionPauseAttributionRecorderTest(unittest.TestCase):
    """The recorder that makes a latency tail spike attributable to a GC pause."""

    def test_a_forced_collection_is_recorded_and_exported(self):
        recorder = GarbageCollectionPauseAttributionRecorder()
        with recorder:
            gc.collect()
        recorded_event_count = recorder.recorded_collection_phase_event_count()
        self.assertGreater(recorded_event_count, 0)

        with tempfile.TemporaryDirectory() as export_directory:
            export_path = os.path.join(export_directory, "gc-collections.jsonl")
            written_line_count = recorder.export_collection_records_as_jsonl(export_path)
            self.assertEqual(written_line_count, recorded_event_count)
            with open(export_path, encoding="utf-8") as exported_records_file:
                self.assertEqual(
                    len(exported_records_file.read().splitlines()), written_line_count
                )

    def test_no_events_are_recorded_after_the_callback_is_uninstalled(self):
        recorder = GarbageCollectionPauseAttributionRecorder()
        with recorder:
            gc.collect()
        recorded_event_count_at_uninstall = (
            recorder.recorded_collection_phase_event_count()
        )
        gc.collect()
        self.assertEqual(
            recorder.recorded_collection_phase_event_count(),
            recorded_event_count_at_uninstall,
        )


if __name__ == "__main__":
    unittest.main()
