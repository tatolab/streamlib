# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Python processor holds a tensor storage buffer and hands it to torch with
no copy; another processor resolves it by surface id and reads the same values.

Both run in processor interpreters of their own beneath `tatolabd`, and report
over the `MARKER:PROBE_RESULT` lines those interpreters forward to its stderr.
The export is `kDLCUDA` on Linux and `kDLMetal` on macOS, where MLX reads the
same tensor too; a rig whose torch sees no such device skips.
"""

import os
import sys
from collections.abc import Callable

import pytest

import tensor_storage_buffer_streams
from runtime_process_under_test import RuntimeProcessUnderTest
from tensor_storage_buffer_probes import (
    CONTROL_PAINT_COLOUR,
    FRAMES_PUBLISHED_WHILE_THE_FIRST_IS_HELD,
    INDEX_PATTERN_TENSOR_BINDING,
    KERNEL_WRITTEN_TENSOR_SHAPE,
    MODEL_INPUT_TENSOR_SHAPE,
    NATURAL_TORCH_DEVICE_TYPE,
    ODD_TENSOR_SHAPE,
    PAINT_COLOUR,
    PAINT_COLOUR_BINDING,
    POOL_ROTATION_DEPTH,
)

pytestmark = pytest.mark.requires_gpu


def run_scenario(
    start_tatolabd_running_stream: "Callable[..., RuntimeProcessUnderTest]",
    scenario: str,
    awaited_reports: int,
    extra_environment: "dict[str, str] | None" = None,
) -> dict:
    """Run one scenario to completion, and return its reports by probe name;
    skip when the producer found no torch device to hand the tensor to."""
    tatolabd = start_tatolabd_running_stream(
        tensor_storage_buffer_streams.STREAM_BY_SCENARIO[scenario],
        extra_environment=extra_environment,
    )
    for report_number in range(awaited_reports):
        report = tatolabd.await_marker("PROBE_RESULT", occurrence=report_number + 1)
        if isinstance(report, dict) and (
            "torch_device_unavailable" in report or "failure" in report
        ):
            break
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    reports_by_probe: "dict[str, list[dict]]" = {}
    for report in tatolabd.marker_payloads("PROBE_RESULT"):
        assert isinstance(report, dict), f"no parseable probe result:\n{tatolabd.stderr_text}"
        if "failure" in report:
            pytest.fail(
                f"{report['probe']} raised in its processor interpreter:\n{report['failure']}"
            )
        if "torch_device_unavailable" in report:
            pytest.skip(
                f"no torch device on this rig: {report['torch_device_unavailable']}"
            )
        reports_by_probe.setdefault(report["probe"], []).append(report)
    return reports_by_probe


def environment_reaching_the_window_server() -> "dict[str, str]":
    """A relative `WAYLAND_DISPLAY` made absolute, because tatolabd's private
    `XDG_RUNTIME_DIR` does not hold the compositor's socket."""
    wayland_display = os.environ.get("WAYLAND_DISPLAY")
    session_runtime_directory = os.environ.get("XDG_RUNTIME_DIR")
    if not wayland_display or os.path.isabs(wayland_display) or not session_runtime_directory:
        return {}
    return {"WAYLAND_DISPLAY": os.path.join(session_runtime_directory, wayland_display)}


def slot_of(surface_id: str) -> str:
    """The pool slot a published `<slot>#<generation>` id names."""
    return surface_id.rsplit("#", 1)[0]


def assert_written_through_torch_with_no_copy(producer_report: dict, shape) -> None:
    for observation in producer_report["producer_observations"]:
        assert observation["tensor_device"].startswith(NATURAL_TORCH_DEVICE_TYPE), (
            observation
        )
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
    start_tatolabd_running_stream,
):
    """(1, 3, 640, 640) float32: written in one processor interpreter through
    `torch.from_dlpack`, resolved by id in another, values equal."""
    reports = run_scenario(
        start_tatolabd_running_stream,
        "a_written_tensor_is_read_by_another_process",
        POOL_ROTATION_DEPTH + 1,
    )
    producer = reports["TensorStorageBufferPublishingSource"][0]
    reads = reports["PublishedTensorReadingSink"]

    assert_written_through_torch_with_no_copy(producer, MODEL_INPUT_TENSOR_SHAPE)
    assert [read["surface_id"] for read in reads] == producer["surface_ids_published"]
    for read in reads:
        assert read["tensor_device"].startswith(NATURAL_TORCH_DEVICE_TYPE), read
        assert read["stated_shape"] == MODEL_INPUT_TENSOR_SHAPE
        assert read["stated_dtype"] == "float32"
        assert read["tensor_shape"] == MODEL_INPUT_TENSOR_SHAPE
        assert read["values_equal"], (
            f"frame {read['frame_index']} read back other values than its producer wrote"
        )
    assert producer["pid"] not in {read["pid"] for read in reads}


def test_an_odd_shaped_tensor_round_trips(start_tatolabd_running_stream):
    """(3, 7, 11) float16 — 462 bytes, no page multiple: CUDA maps the tensor's
    exact byte size, never the allocation's rounded one, and on macOS the
    capsule spans the tensor alone over its IOSurface's one 16 KiB row."""
    reports = run_scenario(
        start_tatolabd_running_stream,
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
    start_tatolabd_running_stream,
):
    """The negative control: the comparison is not vacuous — the previous
    frame's tensor does not carry this frame's values."""
    reports = run_scenario(
        start_tatolabd_running_stream,
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


def test_a_tensor_a_consumer_holds_is_never_rewritten(start_tatolabd_running_stream):
    """The consumer holds the first tensor open while the producer publishes
    several pool depths past it: its values stay frame 0's, and its slot is
    never published from again."""
    reports = run_scenario(
        start_tatolabd_running_stream,
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
def test_a_tensor_acquired_after_a_window_opens_round_trips(start_tatolabd_running_stream):
    """DEVICE_LOCAL OPAQUE_FD memory allocated after a swapchain exists still
    allocates and round-trips (docs/learnings/nvidia-opaque-fd-after-swapchain.md)."""
    reports = run_scenario(
        start_tatolabd_running_stream,
        "a_tensor_acquired_after_a_window_opens_round_trips",
        POOL_ROTATION_DEPTH + 1,
        extra_environment=environment_reaching_the_window_server(),
    )
    for read in reports["PublishedTensorReadingSink"]:
        assert read["values_equal"], read


def test_a_kernel_writes_a_tensor_bound_by_surface_id_and_a_draw_reads_one(
    start_tatolabd_running_stream,
):
    """A compute kernel fills a tensor with an index pattern and
    `torch.from_dlpack` sees exactly that pattern; a tensor the kernel never
    named keeps its sentinel. A draw paints the colour its bound tensor holds,
    and a second tensor paints a second colour."""
    reports = run_scenario(
        start_tatolabd_running_stream, "a_kernel_binds_a_tensor_by_surface_id", 1
    )
    observed = reports["TensorStorageBufferKernelBindingProbe"][0]

    assert observed["compute_binding_names"] == [INDEX_PATTERN_TENSOR_BINDING]
    assert observed["dispatched_tensor_device"].startswith(
        NATURAL_TORCH_DEVICE_TYPE
    ), observed
    assert observed["dispatched_tensor_shape"] == KERNEL_WRITTEN_TENSOR_SHAPE
    assert observed["dispatched_holds_the_index_pattern"], (
        "torch did not read the index pattern the kernel wrote"
    )
    assert not observed["undispatched_holds_the_index_pattern"], (
        "a tensor no dispatch named carries the pattern: the comparison is vacuous"
    )
    assert observed["undispatched_still_holds_the_sentinel"]
    assert observed["foreign_device_refusal"] is not None, (
        "a tensor exported a capsule for a DLPack device it does not live on"
    )
    assert "was requested" in observed["foreign_device_refusal"]

    def rgba8(colour):
        return [round(channel * 255) for channel in colour]

    assert observed["graphics_binding_names"] == [PAINT_COLOUR_BINDING]
    assert observed["distinct_pixels_painted_from_the_tensor"] == [
        rgba8(PAINT_COLOUR)
    ]
    assert observed["distinct_pixels_painted_from_the_control_tensor"] == [
        rgba8(CONTROL_PAINT_COLOUR)
    ]


@pytest.mark.skipif(sys.platform != "darwin", reason="MLX reads kDLMetal on macOS")
@pytest.mark.parametrize(
    "scenario, shape",
    [
        ("a_written_tensor_is_read_by_another_process", MODEL_INPUT_TENSOR_SHAPE),
        ("an_odd_shaped_tensor_round_trips", ODD_TENSOR_SHAPE),
    ],
)
def test_mlx_reads_the_tensor_torch_mps_wrote_in_another_process(
    start_tatolabd_running_stream, scenario, shape
):
    """The reader hands the resolved tensor to MLX as well as torch: MLX sees
    the values torch-MPS wrote in the producer's process, over the same
    IOSurface pages."""
    reports = run_scenario(start_tatolabd_running_stream, scenario, POOL_ROTATION_DEPTH + 1)
    reads = reports["PublishedTensorReadingSink"]
    assert reads, "the sink resolved no tensor"
    for read in reads:
        assert "mlx_values_equal" in read, (
            "mlx is not installed in this venv; it is a darwin test dependency"
        )
        assert read["mlx_shape"] == shape
        assert read["mlx_values_equal"], (
            f"MLX read other values than frame {read['frame_index']}'s"
        )
