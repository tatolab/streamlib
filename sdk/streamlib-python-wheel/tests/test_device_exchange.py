# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The device half of the pixel exchange: graph frames as GPU tensors.

A frame published into the graph reaches CUDA through one engine-side blit into
an exportable staging buffer — zero CPU copies, and CUDA never allocates. The
consumer writes ordinary user code: resolve the frame, `torch.from_dlpack`,
work on a CUDA tensor.

Every processor runs in its own helper process, so the staging lives one
process away: the child imports it over the surface-share check-out and waits
on the staging's refill timeline for each copy the parent runs. What is worth
breaking a build over is that the tensor really is device-resident, that its
pixels are the frame's pixels, that a device-side edit publishes back at
unlock, and that a tensor outliving its handle keeps every layer of the mapping
alive.

Every probe runs in its own helper process and reports one
`MARKER:PROBE_RESULT` JSON line; the tests drive the app out of process and
assert on that line.

These need an NVIDIA driver as well as a GPU; a rig without one skips, because
the CPU fallback is itself under test.
"""

import json
import re
from pathlib import Path

import pytest

from device_exchange_probes import (
    DLPACK_DEVICE_CUDA,
    SURFACE_HEIGHT,
    SURFACE_WIDTH,
)

pytestmark = pytest.mark.requires_gpu

APP = Path(__file__).parent / "device_exchange_app.py"

PROBE_RESULT = re.compile(r"MARKER:PROBE_RESULT (\{.*\})")


def run_probe(start_app_under_test, scenario: str) -> dict:
    """One scenario, one observation dict — or a failure carrying the probe's
    own traceback, which names the cause better than a missing marker."""
    app = start_app_under_test(APP, scenario)
    app.await_output_containing("MARKER:PROBE_RESULT", f"the {scenario} probe's result")
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()
    match = PROBE_RESULT.search(app.output)
    assert match is not None, f"no parseable probe result:\n{app.output}"
    observation = json.loads(match.group(1))
    if "failure" in observation:
        pytest.fail(f"the probe raised in its helper process:\n{observation['failure']}")
    return observation


def skip_without_cuda(observation: dict) -> None:
    reason = observation.get("cuda_unavailable")
    if reason:
        pytest.skip(f"no usable CUDA runtime on this rig: {reason}")


# ---------------------------------------------------------------------------
# The headline: a graph frame is a CUDA tensor
# ---------------------------------------------------------------------------


def test_a_graph_frame_reaches_torch_as_a_cuda_tensor(start_app_under_test):
    """The ticket's headline, in the user's own spelling.

    `torch.from_dlpack(resolved_frame)` yields a GPU-resident tensor whose
    pixels are the frame's pixels — one engine-side blit, no CPU copy, no
    user-facing buffer flavour to pick — from a processor that does not share
    the engine's address space.
    """
    observation = run_probe(start_app_under_test, "GraphFrameToTorchProbe")
    skip_without_cuda(observation)
    assert observation["reported_device"][0] == DLPACK_DEVICE_CUDA
    assert observation["tensor_device"].startswith("cuda")
    assert observation["tensor_shape"] == [SURFACE_HEIGHT, SURFACE_WIDTH, 4]
    assert observation["tensor_dtype"] == "torch.uint8"
    assert observation["pixels_match_host"], (
        "the CUDA tensor's pixels differ from the frame's — the blit exported the wrong memory"
    )


# ---------------------------------------------------------------------------
# In-place edit: device write publishes back at unlock
# ---------------------------------------------------------------------------


def test_a_device_side_edit_publishes_back_to_the_surface_at_unlock(
    start_app_under_test,
):
    """The mutate-in-place demo, on the GPU.

    torch writes the staging buffer; `unlock()` asks the parent to copy it back
    into the frame's own allocation, so a second, independent resolve observes
    the edit. Mental-revert: drop the copy-back from unlock and this reads the
    original pattern.
    """
    observation = run_probe(start_app_under_test, "DeviceEditProbe")
    skip_without_cuda(observation)
    assert observation["pixel_after_publish"] == [17, 34, 51, 68]
    assert observation["cleared_pixel"] == [0, 0, 0, 0]


def test_a_device_edit_survives_the_with_block_spelling(start_app_under_test):
    """close() publishes pending device writes too.

    A `with` block that never calls unlock reaches close() directly; an edit
    silently discarded there is data loss in the API's own idiomatic spelling.
    """
    observation = run_probe(start_app_under_test, "WithBlockEditProbe")
    skip_without_cuda(observation)
    assert observation["pixel_after_with_block"] == [99, 88, 77, 66]


# ---------------------------------------------------------------------------
# Lifetime across every layer
# ---------------------------------------------------------------------------


def test_a_device_tensor_outliving_its_handle_keeps_a_live_mapping(
    start_app_under_test,
):
    """Use-after-free across the engine allocation, the staging and the CUDA
    import — closing the handle frees none of them while a tensor lives."""
    observation = run_probe(start_app_under_test, "TensorOutlivesHandleProbe")
    skip_without_cuda(observation)
    assert observation["checksum_after"] == observation["checksum_before"]


# ---------------------------------------------------------------------------
# The explicit host side
# ---------------------------------------------------------------------------


def test_the_host_side_stays_reachable_on_explicit_request(start_app_under_test):
    """`dl_device=(1, 0)` — numpy's `device="cpu"` — still yields the host
    mapping when a device side exists; `as_numpy` rides the same request."""
    observation = run_probe(start_app_under_test, "HostSideProbe")
    assert observation["host_shape"] == [SURFACE_HEIGHT, SURFACE_WIDTH, 4]
    assert observation["as_numpy_shape"] == [SURFACE_HEIGHT, SURFACE_WIDTH, 4]
    assert observation["same_pixels"]


# ---------------------------------------------------------------------------
# The rotating producer
# ---------------------------------------------------------------------------


def test_camera_device_pixels_match_host_across_ring_cycles(start_app_under_test):
    """Regression lock on the stale-blit-source bug, and on the frame itself.

    Two claims. The camera registers its transient ring texture under each
    frame's surface id, and the export used to resolve texture-first — so a
    cross-process GPU view read the slot the camera had already overwritten
    while the CPU view read the pooled member. The two views must agree.

    Agreement is necessary and not sufficient, which is why this test was
    skipped rather than trusted: two views of one recycled slot agree by
    construction, so identity alone cannot fail for the reason #1755 exists.
    The second claim is the real one — a consumer holding a frame while the
    producer runs 16 frames past it still reads the pixels it was delivered,
    because the checkout lease keeps the pool off that slot.
    """
    if not Path("/dev/video0").exists():
        pytest.skip("no camera on this rig")
    observation = run_probe(start_app_under_test, "camera")
    skip_without_cuda(observation)

    mismatches = [i for i, ok in enumerate(observation["comparisons"]) if not ok]
    assert not mismatches, (
        f"device pixels diverged from host pixels on frames {mismatches} — the blit "
        f"exported a stale ring texture"
    )

    if not observation["a_later_frame_differed"]:
        pytest.skip(
            "every frame this camera produced was identical, so a frame staying still "
            "proves nothing — point the rig at a scene that moves"
        )
    assert observation["held_frame_unchanged"], (
        f"the pixels under a held surface id changed while the producer ran "
        f"{observation['frames_the_producer_ran_ahead']} frames past it — its pool slot "
        f"was rehanded despite the checkout lease"
    )


# ---------------------------------------------------------------------------
# DMA-BUF — the other dialect native code speaks
# ---------------------------------------------------------------------------


def test_a_child_exports_its_surfaces_dma_buf_without_asking_the_parent(
    start_app_under_test,
):
    """The shape third-party native code gets: an fd it can hand to EGL, a V4L2
    output device, or another process.

    The child answers from the fds it was already handed at check-out, so this
    costs no round trip. The import direction refuses by name — a foreign fd
    has to reach the surface registry in the app process, which needs a wire
    that carries an fd.
    """
    observation = run_probe(start_app_under_test, "DmaBufExportProbe")
    assert observation["fd_is_real"]
    assert observation["byte_size"] == observation["expected_byte_size"]
    assert observation["fd_closes_cleanly"]
    assert "export_dma_buf" in observation["import_refusal"], (
        f"the import refusal should point at the direction that does work: "
        f"{observation['import_refusal']!r}"
    )


# ---------------------------------------------------------------------------
# The privileged capability, from a helper process
# ---------------------------------------------------------------------------


def test_the_privileged_capability_works_from_a_helper_process(start_app_under_test):
    """`ctx.gpu_full_access` is reachable from a `setup` hook running in a
    child: each method is its own escalate round trip to the parent, which runs
    the privileged work against the engine and answers with a real surface.

    `escalate(callback)` is the one thing that refuses, and for a reason the
    message has to carry: what cannot cross the boundary is the callback's
    atomic scope, not the privileged operations — those are all still here.
    """
    observation = run_probe(start_app_under_test, "PrivilegedCapabilityProbe")
    assert observation["privileged_acquire_shape"] == [SURFACE_HEIGHT, SURFACE_WIDTH, 4]
    assert observation["privileged_surface_id"]
    assert observation["waited_for_device_idle"]
    assert "atomic" in observation["escalate_refusal"], (
        f"the escalate refusal should say what actually cannot cross: "
        f"{observation['escalate_refusal']!r}"
    )
    assert observation["acquired_texture_surface_id"], (
        "a device texture acquires from a helper process and carries the "
        "surface id a kernel dispatch binds"
    )
    assert observation["acquired_texture_extent"] == [SURFACE_WIDTH, SURFACE_HEIGHT]
