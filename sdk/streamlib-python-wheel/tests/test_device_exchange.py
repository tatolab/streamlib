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
    FILL_CONSTANT_RGBA,
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
    because the checkout lease keeps the pool off that slot. The probe takes
    that lease by reading `into=VideoFrame`; reading untyped and casting
    afterwards takes no claim, which is how this test spent #1869 asserting a
    lease it never held.

    Only the *held* frame is protected. A later frame can recycle before this
    deliberately-slow consumer reaches it — publish-to-claim transit rides pool
    depth — so those are skipped rather than failed, and the count guard below
    keeps that tolerance from emptying the comparison.
    """
    if not Path("/dev/video0").exists():
        pytest.skip("no camera on this rig")
    observation = run_probe(start_app_under_test, "camera")
    skip_without_cuda(observation)

    # A later frame can recycle before this slow consumer reads it, which is
    # the lifetime contract rather than a fault — those are skipped, not failed.
    # Guarding the count keeps that tolerance from quietly emptying the
    # comparison out: an all-recycled run would satisfy `not mismatches`
    # vacuously and stop locking #1755 at all.
    assert len(observation["comparisons"]) >= observation["frames_the_producer_ran_ahead"] // 2, (
        f"only {len(observation['comparisons'])} of "
        f"{observation['frames_the_producer_ran_ahead']} frames were readable "
        f"({observation['frames_recycled_before_this_probe_read_them']} recycled before "
        f"this probe reached them) — too few to lock the blit source"
    )
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


def test_a_dma_buf_fd_round_trips_out_of_and_back_into_the_graph(
    start_app_under_test,
):
    """The shape third-party native code gets: an fd it can hand to EGL, a V4L2
    output device, or another process — and the way one comes back.

    Export answers from the fds the checkout delivered, costing no round trip.
    Import adopts the fd as a fresh registration the graph can resolve; the
    adopted mapping reads the very pixels the exporter wrote, so the handle is
    genuinely the same memory, not a copy.
    """
    observation = run_probe(start_app_under_test, "DmaBufExportProbe")
    assert observation["fd_is_real"]
    assert observation["byte_size"] == observation["expected_byte_size"]
    assert observation["fd_closes_cleanly"]
    assert observation["adopted_pixel"] == [21, 43, 65, 87], (
        f"the adopted surface must map the exporter's memory: "
        f"{observation['adopted_pixel']!r}"
    )
    assert observation["adopted_surface_id"] != observation["exported_surface_id"], (
        "an adoption is a fresh registration, not an alias of the exporter's id"
    )


def test_a_texture_handle_round_trips_across_the_process_boundary(
    start_app_under_test,
):
    """A kernel output reaches a consumer as the texture itself, both handle
    flavours where the format allows.

    The OPAQUE_FD flavour resolves — the child rebuilt the tiled image on its
    own device — and the kernel's pixels read back through the device export,
    which only happens when the layout chain (dispatch publish, checkout,
    acquire barrier, staging refill) named the truth at every step. Its CPU
    accessors and DMA-BUF export refuse by naming the backing. The
    render-target flavour is kernel-written too and exports the fd native
    code imports — the demo shape. A second resolve after release proves the
    round trip left the frame usable.
    """
    observation = run_probe(start_app_under_test, "TextureHandleRoundTripProbe")
    assert observation["limited_surface_mints_no_raw_handle"], (
        "raw handles mint only via the Full surface, on every path"
    )
    assert observation["kernel_dispatched"]
    assert observation["opaque_resolved_extent"] == [SURFACE_WIDTH, SURFACE_HEIGHT]
    assert observation["opaque_resolved_format"] == "rgba8_unorm"
    assert observation["opaque_device_pixel"] == FILL_CONSTANT_RGBA, (
        f"the imported texture must read the kernel's own pixels: "
        f"{observation['opaque_device_pixel']!r}"
    )
    assert "texture-backed" in observation["opaque_pixel_refusal"], (
        f"the pixel refusal should name the tiled backing: "
        f"{observation['opaque_pixel_refusal']!r}"
    )
    assert "OPAQUE_FD" in observation["opaque_export_refusal"], (
        f"the export refusal should name the handle flavour: "
        f"{observation['opaque_export_refusal']!r}"
    )
    assert "export_opaque_fd" in observation["opaque_export_refusal"], (
        f"the refusal must point at the door that answers: "
        f"{observation['opaque_export_refusal']!r}"
    )
    assert observation["opaque_export_fd_is_real"]
    export_metadata = observation["opaque_export_metadata"]
    assert export_metadata["width"] == SURFACE_WIDTH
    assert export_metadata["height"] == SURFACE_HEIGHT
    assert export_metadata["format"] == "rgba8_unorm"
    assert export_metadata["allocation_byte_size"] >= SURFACE_WIDTH * SURFACE_HEIGHT * 4, (
        "the whole-VkDeviceMemory size can pad past the tight size but never "
        "under it"
    )
    # The recipe constants a conforming foreign re-import must reproduce —
    # `new_opaque_fd_export`'s hardcoded shape, read back off the wire.
    assert export_metadata["vk_image_tiling"] == 0  # VK_IMAGE_TILING_OPTIMAL
    assert export_metadata["vk_image_usage_flags"] == 0x0F, (
        "TRANSFER_SRC | TRANSFER_DST | SAMPLED | STORAGE"
    )
    assert export_metadata["vk_image_mip_levels"] == 1
    assert export_metadata["vk_image_array_layers"] == 1
    assert export_metadata["vk_image_samples"] == 1
    assert export_metadata["dedicated_allocation"] is True
    assert export_metadata["vk_memory_type_index"] < 32, "VK_MAX_MEMORY_TYPES"
    assert len(export_metadata["exporting_device_uuid_hex"]) == 32
    assert export_metadata["exporting_device_uuid_hex"] != "00" * 16, (
        "an all-zero device UUID binds no device"
    )
    assert observation["opaque_export_fd_closes_cleanly"]
    assert "Resolve its surface id" in observation["unresolved_export_refusal"], (
        f"an acquired-by-name texture holds no fd child-side: "
        f"{observation['unresolved_export_refusal']!r}"
    )
    assert observation["opaque_second_resolve_extent"] == [
        SURFACE_WIDTH,
        SURFACE_HEIGHT,
    ]
    assert observation["rt_export_fd_is_real"]
    assert observation["rt_export_byte_size"] > 0
    assert observation["rt_fd_closes_cleanly"]
    assert "export_dma_buf" in observation["dma_buf_flavour_export_refusal"], (
        f"the DMA-BUF flavour's refusal must mirror the redirect: "
        f"{observation['dma_buf_flavour_export_refusal']!r}"
    )
    assert "export_dma_buf" in observation["pixel_buffer_export_refusal"], (
        f"a pixel buffer's memory fd has a door, and the refusal names it: "
        f"{observation['pixel_buffer_export_refusal']!r}"
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


# ---------------------------------------------------------------------------
# The scoped device-tensor view over a kernel output
# ---------------------------------------------------------------------------


def test_a_kernel_output_doubles_in_place_through_the_device_tensor_scope(
    start_app_under_test,
):
    """The demo, in the user's own spelling: `with output.as_device_tensor()
    as tensor: torch.from_dlpack(tensor).mul_(2.0)`.

    The rgba16_float output reaches torch as float16 — the float-format
    acceptance this ticket adds — and the doubled values survive a second
    scope entry, whose blit re-reads the engine's texture: the write-back
    landed in the texture, not just the staging.
    """
    observation = run_probe(
        start_app_under_test, "DeviceTensorScopeDoublesAKernelOutputProbe"
    )
    skip_without_cuda(observation)
    assert observation["tensor_dtype"] == "torch.float16"
    assert observation["tensor_shape"] == [SURFACE_HEIGHT, SURFACE_WIDTH, 4]
    assert observation["tensor_device"].startswith("cuda")
    assert observation["filled_pixel"] == [0.25, 0.5, 1.5, 2.0]
    assert observation["doubled_pixel"] == [0.5, 1.0, 3.0, 4.0], (
        "the in-place edit must survive the scope's blit-back into the texture"
    )


def test_a_raise_inside_the_device_tensor_scope_discards_the_write(
    start_app_under_test,
):
    """Owner decision 2026-08-07: leaving the scope by a propagating exception
    discards the write — the engine's texture keeps the kernel output it
    already held, the exception is not suppressed, and the surface (and the
    kernel writing it) keep working on the next frame.
    """
    observation = run_probe(start_app_under_test, "DeviceTensorScopeDiscardsOnRaiseProbe")
    skip_without_cuda(observation)
    assert observation["exception_propagated"] == "deliberate mid-scope failure"
    assert observation["pixel_after_raise"] == FILL_CONSTANT_RGBA, (
        f"a raise mid-scope must leave the pre-scope pixels in place: "
        f"{observation['pixel_after_raise']!r}"
    )
    assert observation["pixel_after_redispatch"] == FILL_CONSTANT_RGBA


def test_a_raise_inside_the_pixel_buffer_scope_discards_the_write(
    start_app_under_test,
):
    """One rule for both scopes (owner, 2026-08-07): the CPU pixel-buffer
    scope stops publishing a pending device write when the block is left by
    a raise. This deliberately changes shipped behaviour — two scopes with
    two behaviours is not shippable.
    """
    observation = run_probe(start_app_under_test, "PixelBufferScopeDiscardsOnRaiseProbe")
    skip_without_cuda(observation)
    assert observation["exception_propagated"] == "deliberate mid-scope failure"
    assert observation["pixel_after"] == observation["pixel_before"], (
        "a raise inside the with-block must leave the frame's pixels untouched"
    )


def test_a_pooled_texture_exports_a_device_tensor(start_app_under_test):
    """Resurrected from #1737 (removed by #1754, carried by #1757): the
    texture-first blit arm serves an acquired pooled texture through the
    handle itself — registration at acquire keys the export, and the tensor
    is device memory of the texture's extent."""
    observation = run_probe(start_app_under_test, "PooledTextureExportProbe")
    skip_without_cuda(observation)
    assert observation["texture_surface_id"]
    if observation["texture_device"][0] != DLPACK_DEVICE_CUDA:
        pytest.skip(f"no usable CUDA runtime: {observation['texture_device']}")
    assert observation["texture_tensor_shape"] == [SURFACE_HEIGHT, SURFACE_WIDTH, 4]
    assert observation["texture_tensor_device"].startswith("cuda")


def test_a_texture_whose_usage_forbids_the_copy_refuses_at_scope_entry(
    start_app_under_test,
):
    """The engine refuses a copy the Vulkan spec forbids instead of recording
    it and letting the driver silently tolerate UB: a sampled-only texture
    cannot blit out, a copy_dst-less one cannot take the blit back, and both
    refuse at `__enter__` naming the usage to add.
    """
    observation = run_probe(
        start_app_under_test, "DeviceTensorScopeRefusesAnUnexportableUsageProbe"
    )
    skip_without_cuda(observation)
    assert "copy_src" in observation["copy_src_refusal"], (
        f"the blit-out refusal must name the missing usage: "
        f"{observation['copy_src_refusal']!r}"
    )
    assert "copy_dst" in observation["copy_dst_refusal"], (
        f"the blit-back refusal must name the missing usage: "
        f"{observation['copy_dst_refusal']!r}"
    )
