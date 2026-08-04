# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The device half of the pixel exchange, proven against a real GPU.

An exchange buffer's memory is allocated by the engine, imported by CUDA, and
handed to a framework as a DLPack tensor — no copy, and CUDA never allocates.
What is worth breaking a build over here is the import landing on the right
device, the capsule reporting the memory it actually got, and the tensor
outliving its handle across two allocators rather than one.

These need an NVIDIA driver as well as a GPU; a rig without one skips rather
than fails, because the refusal path is itself covered below.
"""

import queue
import threading
import time

import pytest

import streamlib
from streamlib import RuntimeContextFullAccess, processor

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


def _run_probe(probe_class: type, matching: str) -> dict:
    graph = RunningGraph()
    graph.runtime.add(probe_class)
    graph.start()
    deadline = time.monotonic() + PIPELINE_TIMEOUT_SECONDS
    try:
        while True:
            remaining = deadline - time.monotonic()
            assert remaining > 0, f"no {matching!r} observation arrived within the timeout"
            try:
                observation = _hook_observations.get(timeout=min(0.5, remaining))
            except queue.Empty:
                continue
            if observation.get("observed") == matching:
                break
    finally:
        graph.shut_down()
    if "failure" in observation:
        pytest.fail(f"the probe raised:\n{observation['failure']}")
    return observation


def _skip_without_cuda_driver(observation: dict) -> None:
    reason = observation.get("cuda_unavailable")
    if reason:
        pytest.skip(f"no usable CUDA driver on this rig: {reason}")


# ---------------------------------------------------------------------------
# The export itself
# ---------------------------------------------------------------------------


def _host_request_outcome(surface_handle) -> str:
    """What asking a device-local surface for its host side does."""
    try:
        surface_handle.__dlpack__(max_version=(1, 0), dl_device=(DLPACK_DEVICE_CPU, 0))
    except BufferError:
        return "device-local, as expected"
    return "handed back a host tensor"


@processor(execution="manual")
class DeviceExportProbe:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _observe("device_export", lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        with ctx.gpu_full_access.acquire_device_exchange_buffer(
            SURFACE_WIDTH, SURFACE_HEIGHT
        ) as surface_handle:
            surface_handle.lock(read_only=False)
            try:
                reported_device = surface_handle.__dlpack_device__()
            except RuntimeError as import_failure:
                return {"cuda_unavailable": str(import_failure)}
            capsule = surface_handle.__dlpack__(max_version=(1, 0))
            observation = {
                "reported_device": reported_device,
                "capsule_type": type(capsule).__name__,
                # The import is cached, so a second query must agree with the
                # first — two imports of one allocation would be two mappings,
                # and freeing either would strand the other.
                "device_on_second_query": surface_handle.__dlpack_device__(),
                # The host side is still reachable on request, which is what
                # `as_numpy` relies on for a non-device-local buffer.
                "host_request_refused": _host_request_outcome(surface_handle),
            }
            surface_handle.unlock()
            return observation


def test_an_exchange_buffer_exports_device_memory_to_dlpack():
    """The reported device is the one the driver actually mapped onto.

    `__dlpack_device__` performs the import rather than predicting its
    outcome, because `cudaPointerGetAttributes` is what distinguishes true
    device memory from a driver that quietly downgraded to pinned host
    memory — and a consumer trusts this answer before it ever looks at the
    capsule.
    """
    observation = _run_probe(DeviceExportProbe, "device_export")
    _skip_without_cuda_driver(observation)
    assert observation["capsule_type"] == "PyCapsule"
    device_type, device_ordinal = observation["reported_device"]
    assert device_type == DLPACK_DEVICE_CUDA, (
        f"the imported pointer was classified as device type {device_type}, not CUDA device "
        f"memory — the driver downgraded the import"
    )
    assert device_ordinal >= 0
    assert observation["device_on_second_query"] == observation["reported_device"]
    assert observation["host_request_refused"] == "device-local, as expected"


# ---------------------------------------------------------------------------
# The round trip a user actually writes
# ---------------------------------------------------------------------------


@processor(execution="manual")
class TorchRoundTripProbe:
    """Writes through the host view, reads back as a torch CUDA tensor."""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _observe("torch_round_trip", lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        import torch

        # `device_local=False` keeps a host mapping alongside the
        # device-importable one, so the same bytes can be seeded from the CPU
        # and then read by CUDA — which is what makes this a round trip rather
        # than two unrelated observations.
        with ctx.gpu_full_access.acquire_device_exchange_buffer(
            SURFACE_WIDTH, SURFACE_HEIGHT, "bgra", False
        ) as surface_handle:
            surface_handle.lock(read_only=False)
            try:
                tensor = torch.from_dlpack(surface_handle)
            except RuntimeError as export_failure:
                return {"cuda_unavailable": str(export_failure)}

            observation = {
                "tensor_device": str(tensor.device),
                "tensor_shape": tuple(tensor.shape),
                "tensor_dtype": str(tensor.dtype),
            }
            # A write on the GPU, read back through the host mapping of the
            # same allocation. If anything copied, this does not survive.
            tensor[:, :, :] = 0
            tensor[4, 6] = torch.tensor(
                [12, 34, 56, 78], dtype=torch.uint8, device=tensor.device
            )
            torch.cuda.synchronize()
            host_view = surface_handle.as_numpy()
            observation["pixel_seen_by_the_host"] = host_view[4, 6].tolist()
            observation["an_untouched_pixel"] = host_view[0, 0].tolist()
            surface_handle.unlock()
            return observation


def test_a_torch_cuda_tensor_and_the_host_view_are_the_same_memory():
    """The headline contract: a GPU write is visible to the CPU with no copy.

    torch writes through a CUDA device pointer; the host mapping of the same
    engine allocation observes it. Two mappings, one allocation — which is the
    whole claim the exchange surface makes.
    """
    pytest.importorskip("torch")
    observation = _run_probe(TorchRoundTripProbe, "torch_round_trip")
    _skip_without_cuda_driver(observation)
    assert observation["tensor_device"].startswith("cuda")
    assert observation["tensor_shape"] == (SURFACE_HEIGHT, SURFACE_WIDTH, 4)
    assert observation["tensor_dtype"] == "torch.uint8"
    assert observation["pixel_seen_by_the_host"] == [12, 34, 56, 78]
    assert observation["an_untouched_pixel"] == [0, 0, 0, 0]


# ---------------------------------------------------------------------------
# Lifetime across two allocators
# ---------------------------------------------------------------------------


@processor(execution="manual")
class TensorOutlivesExchangeBufferProbe:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _observe("tensor_outlives_exchange", lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        import torch

        surface_handle = ctx.gpu_full_access.acquire_device_exchange_buffer(
            SURFACE_WIDTH, SURFACE_HEIGHT
        )
        surface_handle.lock(read_only=False)
        try:
            tensor = torch.from_dlpack(surface_handle)
        except RuntimeError as export_failure:
            return {"cuda_unavailable": str(export_failure)}
        tensor[:, :, :] = 5
        torch.cuda.synchronize()

        # The frame is done with; the tensor is not.
        surface_handle.unlock()
        surface_handle.close()
        del surface_handle

        # Reaching the memory now goes through both the engine allocation and
        # the CUDA import, either of which being freed early would fault here.
        return {
            "sum_after_close": int(tensor.sum().item()),
            "expected_sum": SURFACE_WIDTH * SURFACE_HEIGHT * 4 * 5,
        }


def test_a_device_tensor_outliving_its_handle_keeps_a_live_mapping():
    """Use-after-free across two allocators.

    The capsule owns a share of both the engine allocation and the CUDA
    import, so closing the handle frees neither while a tensor is live.

    Mental-revert: drop the `CudaImportedSurface` from the capsule's owner —
    `cudaDestroyExternalMemory` then runs while torch still holds the pointer.
    """
    pytest.importorskip("torch")
    observation = _run_probe(
        TensorOutlivesExchangeBufferProbe, "tensor_outlives_exchange"
    )
    _skip_without_cuda_driver(observation)
    assert observation["sum_after_close"] == observation["expected_sum"]


# ---------------------------------------------------------------------------
# Refusals
# ---------------------------------------------------------------------------


@processor(execution="manual")
class ExchangeRefusalProbe:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _observe("exchange_refusals", lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        outcomes = {}
        gpu_full = ctx.gpu_full_access

        # A device-local buffer has no host mapping to hand numpy.
        with gpu_full.acquire_device_exchange_buffer(
            SURFACE_WIDTH, SURFACE_HEIGHT
        ) as device_only:
            device_only.lock()
            try:
                device_only.as_numpy()
                outcomes["numpy_on_device_local"] = "returned a view"
            except BufferError as refusal:
                outcomes["numpy_on_device_local"] = str(refusal)
            device_only.unlock()

        # An ordinary pooled pixel buffer is DMA-BUF-flavoured, so it has no
        # OPAQUE_FD to give CUDA. Refusing beats importing nothing.
        with ctx.gpu_limited_access.acquire_pixel_buffer(
            SURFACE_WIDTH, SURFACE_HEIGHT
        ) as pooled:
            pooled.lock()
            outcomes["pooled_buffer_device"] = pooled.__dlpack_device__()
            pooled.unlock()

        try:
            gpu_full.acquire_device_exchange_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, "nv12")
            outcomes["nv12_exchange"] = "allocated"
        except ValueError as refusal:
            outcomes["nv12_exchange"] = str(refusal)
        return outcomes


def test_the_device_path_refuses_what_it_cannot_honour():
    observation = _run_probe(ExchangeRefusalProbe, "exchange_refusals")
    assert "device-local" in observation["numpy_on_device_local"]
    # A pooled buffer never claims CUDA: it has no device import to report.
    assert observation["pooled_buffer_device"] == (DLPACK_DEVICE_CPU, 0)
    assert "one linear allocation" in observation["nv12_exchange"]
