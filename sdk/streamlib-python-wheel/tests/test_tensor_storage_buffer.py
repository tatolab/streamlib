# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Python processor holds a tensor storage buffer and hands it to torch with
no copy; another processor resolves it by surface id and reads the same values.

Both run out of process, each in its own helper, and report over the
`MARKER:PROBE_RESULT` lines their helpers forward. Linux only until the macOS
arm lands (#2431); a rig without CUDA skips, since the export is `kDLCUDA`.
"""

import json
import os
import re
import sys
from pathlib import Path

import pytest

from tensor_storage_buffer_probes import (
    FRAMES_PUBLISHED_WHILE_THE_FIRST_IS_HELD,
    MODEL_INPUT_TENSOR_SHAPE,
    ODD_TENSOR_SHAPE,
    POOL_ROTATION_DEPTH,
)

pytestmark = [
    pytest.mark.requires_gpu,
    pytest.mark.skipif(
        sys.platform != "linux",
        reason="the tensor storage buffer's macOS arm rides #2431",
    ),
]

APP = Path(__file__).parent / "tensor_storage_buffer_app.py"

PROBE_RESULT = re.compile(r"MARKER:PROBE_RESULT (\{.*\})")


def run_scenario(start_app_under_test, scenario: str, awaited_reports: int) -> dict:
    """Run one scenario to completion, and return its reports by probe name;
    skip when the producer found no CUDA device to hand the tensor to."""
    app = start_app_under_test(APP, scenario)
    for report_number in range(awaited_reports):
        app.await_output_containing(
            "MARKER:PROBE_RESULT", f"report {report_number + 1} of {awaited_reports}"
        )
        if "cuda_unavailable" in app.output:
            break
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()

    reports_by_probe: "dict[str, list[dict]]" = {}
    for match in PROBE_RESULT.finditer(app.output):
        report = json.loads(match.group(1))
        if "failure" in report:
            pytest.fail(
                f"{report['probe']} raised in its helper process:\n{report['failure']}"
            )
        if "cuda_unavailable" in report:
            pytest.skip(f"no CUDA device on this rig: {report['cuda_unavailable']}")
        reports_by_probe.setdefault(report["probe"], []).append(report)
    return reports_by_probe


def slot_of(surface_id: str) -> str:
    """The pool slot a published `<slot>#<generation>` id names."""
    return surface_id.rsplit("#", 1)[0]


def assert_written_through_torch_with_no_copy(producer_report: dict, shape) -> None:
    for observation in producer_report["producer_observations"]:
        assert observation["tensor_device"].startswith("cuda"), observation
        assert observation["tensor_shape"] == shape
        assert observation["stated_shape"] == shape
        assert observation["exports_share_memory"], (
            f"two exports of {observation['surface_id']} addressed different memory"
        )
        assert observation["write_visible_through_an_independent_import"], (
            f"a second import of {observation['surface_id']} did not see torch's "
            "write before the close: torch wrote a copy, not the engine's memory"
        )


def test_a_tensor_written_through_torch_is_read_by_another_process(
    start_app_under_test,
):
    """(1, 3, 640, 640) float32: written in one helper through
    `torch.from_dlpack`, resolved by id in another, values equal."""
    reports = run_scenario(
        start_app_under_test,
        "a_written_tensor_is_read_by_another_process",
        POOL_ROTATION_DEPTH + 1,
    )
    producer = reports["TensorStorageBufferPublishingSource"][0]
    reads = reports["PublishedTensorReadingSink"]

    assert_written_through_torch_with_no_copy(producer, MODEL_INPUT_TENSOR_SHAPE)
    assert [read["surface_id"] for read in reads] == producer["surface_ids_published"]
    for read in reads:
        assert read["tensor_device"].startswith("cuda"), read
        assert read["stated_shape"] == MODEL_INPUT_TENSOR_SHAPE
        assert read["stated_dtype"] == "float32"
        assert read["tensor_shape"] == MODEL_INPUT_TENSOR_SHAPE
        assert read["values_equal"], (
            f"frame {read['frame_index']} read back other values than its producer wrote"
        )
    assert producer["pid"] not in {read["pid"] for read in reads}


def test_an_odd_shaped_tensor_round_trips(start_app_under_test):
    """(3, 7, 11) float16 — 462 bytes, no page multiple: CUDA maps the tensor's
    exact byte size, never the allocation's rounded one."""
    reports = run_scenario(
        start_app_under_test,
        "an_odd_shaped_tensor_round_trips",
        POOL_ROTATION_DEPTH + 1,
    )
    assert_written_through_torch_with_no_copy(
        reports["TensorStorageBufferPublishingSource"][0], ODD_TENSOR_SHAPE
    )
    for read in reports["PublishedTensorReadingSink"]:
        assert read["stated_shape"] == ODD_TENSOR_SHAPE
        assert read["stated_dtype"] == "float16"
        assert read["values_equal"], read


def test_a_reader_resolving_a_different_id_sees_different_values(
    start_app_under_test,
):
    """The negative control: the comparison is not vacuous — the previous
    frame's tensor does not carry this frame's values."""
    reports = run_scenario(
        start_app_under_test,
        "a_reader_resolving_a_different_id_sees_different_values",
        POOL_ROTATION_DEPTH,  # the sink skips frame 0, plus the producer's report
    )
    reads = reports["PublishedTensorReadingSink"]
    assert reads, "the sink resolved no earlier tensor"
    for read in reads:
        assert read["surface_id"] != read["published_surface_id"]
        assert not read["values_equal"], (
            f"frame {read['frame_index']}'s values were read from another tensor's id"
        )
        assert read["values_equal_its_own_frames"], (
            f"{read['surface_id']} did not carry the values of the frame it was "
            "published for"
        )


def test_a_tensor_a_consumer_holds_is_never_rewritten(start_app_under_test):
    """The consumer holds the first tensor open while the producer publishes
    several pool depths past it: its values stay frame 0's, and its slot is
    never published from again."""
    reports = run_scenario(
        start_app_under_test,
        "a_held_tensor_is_never_rewritten",
        FRAMES_PUBLISHED_WHILE_THE_FIRST_IS_HELD + 1,
    )
    published = reports["TensorStorageBufferPublishingSource"][0][
        "surface_ids_published"
    ]
    rereads = reports["HeldTensorRereadingSink"]

    assert len(rereads) == FRAMES_PUBLISHED_WHILE_THE_FIRST_IS_HELD
    for reread in rereads:
        assert reread["held_surface_id"] == published[0]
        assert reread["held_values_still_frame_0s"], (
            f"after {reread['later_tensors_seen']} later tensors the held one changed"
        )
    assert all(
        slot_of(surface_id) != slot_of(published[0]) for surface_id in published[1:]
    ), f"the held tensor's slot was published from again: {published}"


@pytest.mark.skipif(
    not (os.environ.get("DISPLAY") or os.environ.get("WAYLAND_DISPLAY")),
    reason="needs a window server: the allocation under test follows a swapchain",
)
def test_a_tensor_acquired_after_a_window_opens_round_trips(start_app_under_test):
    """DEVICE_LOCAL OPAQUE_FD memory allocated after a swapchain exists still
    allocates and round-trips (docs/learnings/nvidia-opaque-fd-after-swapchain.md)."""
    reports = run_scenario(
        start_app_under_test,
        "a_tensor_acquired_after_a_window_opens_round_trips",
        POOL_ROTATION_DEPTH + 1,
    )
    for read in reports["PublishedTensorReadingSink"]:
        assert read["values_equal"], read
