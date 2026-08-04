# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The device half of the pixel exchange: graph frames as GPU tensors.

A frame published into the graph reaches CUDA through one engine-side blit
into an exportable staging buffer — zero CPU copies, and CUDA never
allocates. The consumer writes ordinary user code: resolve the frame,
`torch.from_dlpack`, work on a CUDA tensor. What is worth breaking a build
over is that the tensor really is device-resident, that its pixels are the
frame's pixels, that a device-side edit publishes back into the surface at
unlock, and that a tensor outliving its handle keeps every layer of the
mapping alive.

These need an NVIDIA driver as well as a GPU; a rig without one skips,
because the CPU fallback is itself under test.
"""

import queue
import threading
import time

import numpy
import pytest

import streamlib

# `input` is streamlib's port decorator — the test reads like user code,
# which spells it exactly this way.
from streamlib import (  # noqa: A004
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    TestPatternSource,
    VideoFrame,
    input,
    processor,
)

pytestmark = pytest.mark.requires_gpu

PIPELINE_TIMEOUT_SECONDS = 30.0

SURFACE_WIDTH = 64
SURFACE_HEIGHT = 32

# DLPack device-type discriminants, part of the wire ABI.
DLPACK_DEVICE_CPU = 1
DLPACK_DEVICE_CUDA = 2

_hook_observations: "queue.Queue[dict]" = queue.Queue()


@pytest.fixture(autouse=True)
def clean_observation_queue():
    while True:
        try:
            _hook_observations.get_nowait()
        except queue.Empty:
            break
    yield


class RunningGraph:
    def __init__(self) -> None:
        self.runtime = streamlib.Runtime()
        self._run_loop = threading.Thread(target=self.runtime.run, daemon=True)

    def start(self) -> None:
        self._run_loop.start()

    def shut_down(self) -> None:
        self.runtime.shutdown()
        self._run_loop.join(timeout=PIPELINE_TIMEOUT_SECONDS)
        assert not self._run_loop.is_alive(), "run() never returned after shutdown()"


def _observe(matching: str, probe_body):
    try:
        _hook_observations.put({"observed": matching, **probe_body()})
    except BaseException:  # noqa: BLE001 — re-raised by the test
        import traceback

        _hook_observations.put({"observed": matching, "failure": traceback.format_exc()})


def _await_observation(matching: str) -> dict:
    deadline = time.monotonic() + PIPELINE_TIMEOUT_SECONDS
    while True:
        remaining = deadline - time.monotonic()
        assert remaining > 0, f"no {matching!r} observation arrived within the timeout"
        try:
            observation = _hook_observations.get(timeout=min(0.5, remaining))
        except queue.Empty:
            continue
        if observation.get("observed") == matching:
            return observation


def _run_frame_probe(probe_class: type, matching: str) -> dict:
    """Run TestPatternSource → probe and collect the probe's observation."""
    graph = RunningGraph()
    pattern = graph.runtime.add(
        TestPatternSource, config={"width": SURFACE_WIDTH, "height": SURFACE_HEIGHT}
    )
    probe = graph.runtime.add(probe_class)
    graph.runtime.connect(pattern.output("video"), probe.input("video_from_upstream"))
    graph.start()
    try:
        observation = _await_observation(matching)
    finally:
        graph.shut_down()
    if "failure" in observation:
        pytest.fail(f"the probe raised:\n{observation['failure']}")
    return observation


def _skip_without_cuda(observation: dict) -> None:
    reason = observation.get("cuda_unavailable")
    if reason:
        pytest.skip(f"no usable CUDA runtime on this rig: {reason}")


class _FrameProbeBase:
    """Reads exactly one frame bag, then reports through `_observe`."""

    observation_name = ""

    def _probe(self, ctx: RuntimeContextLimitedAccess, frame: VideoFrame) -> dict:
        raise NotImplementedError

    @input(delivery_profile="every_sample")
    def video_from_upstream(self) -> None: ...

    def __init__(self) -> None:
        self.frames_seen = 0

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        bag = ctx.inputs.read("video_from_upstream")
        if bag is None or self.frames_seen >= 1:
            return
        self.frames_seen += 1
        _observe(self.observation_name, lambda: self._probe(ctx, VideoFrame.from_bag(bag)))


# ---------------------------------------------------------------------------
# The headline: a graph frame is a CUDA tensor
# ---------------------------------------------------------------------------


@processor
class GraphFrameToTorchProbe(_FrameProbeBase):
    observation_name = "graph_frame_to_torch"

    def _probe(self, ctx: RuntimeContextLimitedAccess, frame) -> dict:
        import torch

        with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as surface:
            surface.lock()
            reported_device = surface.__dlpack_device__()
            if reported_device[0] != DLPACK_DEVICE_CUDA:
                surface.unlock()
                return {"cuda_unavailable": f"__dlpack_device__ reported {reported_device}"}
            tensor = torch.from_dlpack(surface)
            host_view = numpy.from_dlpack(surface, device="cpu")
            observation = {
                "reported_device": reported_device,
                "tensor_device": str(tensor.device),
                "tensor_shape": tuple(tensor.shape),
                "tensor_dtype": str(tensor.dtype),
                # The tensor's pixels are the frame's pixels: compare a
                # sample against the host mapping of the same surface.
                "pixels_match_host": bool(
                    (tensor[3, 5].cpu().numpy() == host_view[3, 5]).all()
                ),
            }
            surface.unlock()
            return observation


def test_a_graph_frame_reaches_torch_as_a_cuda_tensor():
    """The ticket's headline, in the user's own spelling.

    `torch.from_dlpack(resolved_frame)` yields a GPU-resident tensor whose
    pixels are the frame's pixels — one engine-side blit, no CPU copy, no
    user-facing buffer flavour to pick.
    """
    pytest.importorskip("torch")
    observation = _run_frame_probe(GraphFrameToTorchProbe, "graph_frame_to_torch")
    _skip_without_cuda(observation)
    assert observation["reported_device"][0] == DLPACK_DEVICE_CUDA
    assert observation["tensor_device"].startswith("cuda")
    assert observation["tensor_shape"] == (SURFACE_HEIGHT, SURFACE_WIDTH, 4)
    assert observation["tensor_dtype"] == "torch.uint8"
    assert observation["pixels_match_host"], (
        "the CUDA tensor's pixels differ from the frame's — the blit exported the wrong memory"
    )


# ---------------------------------------------------------------------------
# In-place edit: device write publishes back at unlock
# ---------------------------------------------------------------------------


@processor
class DeviceEditProbe(_FrameProbeBase):
    observation_name = "device_edit"

    def _probe(self, ctx: RuntimeContextLimitedAccess, frame) -> dict:
        import torch

        gpu = ctx.gpu_limited_access
        with gpu.resolve_surface(frame.surface_id) as surface:
            surface.lock(read_only=False)
            if surface.__dlpack_device__()[0] != DLPACK_DEVICE_CUDA:
                surface.unlock()
                return {"cuda_unavailable": "device side not reachable"}
            tensor = torch.from_dlpack(surface)
            tensor[:, :, :] = 0
            tensor[9, 11] = torch.tensor(
                [17, 34, 51, 68], dtype=torch.uint8, device=tensor.device
            )
            torch.cuda.synchronize()
            # unlock is the publication point for a device-side write.
            surface.unlock()

        with gpu.resolve_surface(frame.surface_id) as reread:
            reread.lock()
            fresh_view = numpy.from_dlpack(reread, device="cpu")
            observation = {
                "pixel_after_publish": fresh_view[9, 11].tolist(),
                "cleared_pixel": fresh_view[0, 0].tolist(),
            }
            reread.unlock()
            return observation


def test_a_device_side_edit_publishes_back_to_the_surface_at_unlock():
    """The mutate-in-place demo, on the GPU.

    torch writes the staging buffer; `unlock()` copies it back into the
    frame's own allocation, so a second, independent resolve observes the
    edit. Mental-revert: drop the copy-back from unlock and the fresh
    resolve reads the original pattern.
    """
    pytest.importorskip("torch")
    observation = _run_frame_probe(DeviceEditProbe, "device_edit")
    _skip_without_cuda(observation)
    assert observation["pixel_after_publish"] == [17, 34, 51, 68]
    assert observation["cleared_pixel"] == [0, 0, 0, 0]


# ---------------------------------------------------------------------------
# Lifetime across three layers
# ---------------------------------------------------------------------------


@processor
class TensorOutlivesHandleProbe(_FrameProbeBase):
    observation_name = "tensor_outlives_handle"

    def _probe(self, ctx: RuntimeContextLimitedAccess, frame) -> dict:
        import torch

        surface = ctx.gpu_limited_access.resolve_surface(frame.surface_id)
        surface.lock()
        if surface.__dlpack_device__()[0] != DLPACK_DEVICE_CUDA:
            surface.unlock()
            return {"cuda_unavailable": "device side not reachable"}
        tensor = torch.from_dlpack(surface)
        checksum_before = int(tensor.to(torch.int64).sum().item())
        surface.unlock()
        surface.close()
        del surface

        # The handle is gone; the tensor must still address live memory —
        # its capsule holds the surface, the staging, and the CUDA import.
        torch.cuda.synchronize()
        checksum_after = int(tensor.to(torch.int64).sum().item())
        return {
            "checksum_before": checksum_before,
            "checksum_after": checksum_after,
        }


def test_a_device_tensor_outliving_its_handle_keeps_a_live_mapping():
    """Use-after-free across the engine allocation, the staging, and the
    CUDA import — closing the handle frees none of them while a tensor
    lives."""
    pytest.importorskip("torch")
    observation = _run_frame_probe(TensorOutlivesHandleProbe, "tensor_outlives_handle")
    _skip_without_cuda(observation)
    assert observation["checksum_after"] == observation["checksum_before"]


# ---------------------------------------------------------------------------
# The explicit host side, and refusals
# ---------------------------------------------------------------------------


@processor
class HostSideProbe(_FrameProbeBase):
    observation_name = "host_side"

    def _probe(self, ctx: RuntimeContextLimitedAccess, frame) -> dict:
        with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as surface:
            surface.lock()
            host_view = numpy.from_dlpack(surface, device="cpu")
            via_as_numpy = surface.as_numpy()
            observation = {
                "host_shape": host_view.shape,
                "as_numpy_shape": via_as_numpy.shape,
                "same_pixels": bool((host_view[2, 2] == via_as_numpy[2, 2]).all()),
            }
            surface.unlock()
            return observation


def test_the_host_side_stays_reachable_on_explicit_request():
    """`dl_device=(1, 0)` — numpy's `device="cpu"` — still yields the host
    mapping when a device side exists; `as_numpy` rides the same request."""
    observation = _run_frame_probe(HostSideProbe, "host_side")
    assert observation["host_shape"] == (SURFACE_HEIGHT, SURFACE_WIDTH, 4)
    assert observation["as_numpy_shape"] == (SURFACE_HEIGHT, SURFACE_WIDTH, 4)
    assert observation["same_pixels"]


@processor(execution="manual")
class NoExportKeyProbe:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _observe("no_export_key", lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        outcomes = {}
        # A pooled texture carries no surface id — nothing keys an export.
        with ctx.gpu_limited_access.acquire_texture(
            SURFACE_WIDTH, SURFACE_HEIGHT, "rgba8_unorm", ["copy_src", "texture_binding"]
        ) as texture_handle:
            outcomes["texture_device"] = texture_handle.__dlpack_device__()

        # An acquired pixel buffer resolves the device path through its id.
        with ctx.gpu_limited_access.acquire_pixel_buffer(
            SURFACE_WIDTH, SURFACE_HEIGHT
        ) as buffer_handle:
            outcomes["acquired_buffer_device"] = buffer_handle.__dlpack_device__()
        return outcomes


def test_export_keying_matches_what_the_surface_carries():
    graph = RunningGraph()
    graph.runtime.add(NoExportKeyProbe)
    graph.start()
    try:
        observation = _await_observation("no_export_key")
    finally:
        graph.shut_down()
    if "failure" in observation:
        pytest.fail(f"the probe raised:\n{observation['failure']}")
    # No id → no device export to attempt; the host mapping answers.
    assert observation["texture_device"] == (DLPACK_DEVICE_CPU, 0)
    # An id + the limited capability → the device side may answer (CUDA)
    # or fall back to the host on a driverless rig; both are legal here.
    assert observation["acquired_buffer_device"][0] in (
        DLPACK_DEVICE_CPU,
        DLPACK_DEVICE_CUDA,
    )


# ---------------------------------------------------------------------------
# DMA-BUF — the other dialect native code speaks
# ---------------------------------------------------------------------------


@processor(execution="manual")
class DmaBufRoundTripProbe:
    """Exports a surface's DMA-BUF fd and imports it back as a second surface."""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _observe("dma_buf_round_trip", lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        gpu_full = ctx.gpu_full_access
        with ctx.gpu_limited_access.acquire_pixel_buffer(
            SURFACE_WIDTH, SURFACE_HEIGHT
        ) as exported_surface:
            exported_surface.lock(read_only=False)
            exported_surface.as_numpy()[:, :, :] = 0
            exported_surface.as_numpy()[7, 9] = [21, 43, 65, 87]
            exported_surface.unlock()

            fd, byte_size = gpu_full.export_dma_buf(exported_surface)
            observation = {
                "fd_is_real": fd >= 0,
                "byte_size": byte_size,
                "expected_byte_size": SURFACE_WIDTH * SURFACE_HEIGHT * 4,
            }
            # The import adopts the fd, so nothing here closes it.
            with gpu_full.import_dma_buf(
                fd, SURFACE_WIDTH, SURFACE_HEIGHT
            ) as imported_surface:
                imported_surface.lock()
                observation["pixel_seen_through_the_import"] = (
                    imported_surface.as_numpy()[7, 9].tolist()
                )
                imported_surface.unlock()
            return observation


def test_a_dma_buf_export_reimports_as_the_same_memory():
    """The export/import pair is a handle to one allocation, not a copy.

    This is the shape third-party native code gets: an fd it can hand to EGL,
    a V4L2 output device, or another process.
    """
    graph = RunningGraph()
    graph.runtime.add(DmaBufRoundTripProbe)
    graph.start()
    try:
        observation = _await_observation("dma_buf_round_trip")
    finally:
        graph.shut_down()
    if "failure" in observation:
        pytest.fail(f"the probe raised:\n{observation['failure']}")
    assert observation["fd_is_real"]
    assert observation["byte_size"] == observation["expected_byte_size"]
    assert observation["pixel_seen_through_the_import"] == [21, 43, 65, 87]
