# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The pixel-exchange surface, proven against a running engine.

Pixels reach Python in place: a numpy view or a DLPack tensor addresses the
same bytes the engine allocated, so a write through the view is visible to
anything else holding that surface. The three contracts worth breaking a build
over are here — the row pitch survives into the strides, an export taken
without a lock is refused, and a tensor that outlives its frame keeps
addressing live memory rather than a recycled pool slot.
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

# What a hook observed, drained by the test after the graph runs. Module-level
# because configuration is JSON on the graph node — a queue cannot travel
# through `config`.
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
    """A graph running on a worker thread, shut down and joined on exit."""

    def __init__(self) -> None:
        self.runtime = streamlib.Runtime()
        self.run_outcome: "queue.Queue[BaseException | None]" = queue.Queue()
        self._run_loop = threading.Thread(target=self._run, daemon=True)

    def _run(self) -> None:
        try:
            self.runtime.run()
        except BaseException as run_failure:  # noqa: BLE001 — surfaced by the caller
            self.run_outcome.put(run_failure)
        else:
            self.run_outcome.put(None)

    def start(self) -> None:
        self._run_loop.start()

    def shut_down(self) -> None:
        self.runtime.shutdown()
        self._run_loop.join(timeout=PIPELINE_TIMEOUT_SECONDS)
        assert not self._run_loop.is_alive(), "run() never returned after shutdown()"


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


def _observe(matching: str, probe_body):
    """Run a probe body, reporting whatever it raises as the observation.

    A hook that raises is logged and swallowed by the host, so without this
    every mistake in a probe arrives as an indistinguishable 30-second timeout.
    """
    try:
        _hook_observations.put({"observed": matching, **probe_body()})
    except BaseException as probe_failure:  # noqa: BLE001 — re-raised by the test
        import traceback

        _hook_observations.put(
            {"observed": matching, "failure": traceback.format_exc()}
        )
        del probe_failure


def _run_probe(probe_class: type, matching: str) -> dict:
    graph = RunningGraph()
    graph.runtime.add(probe_class)
    graph.start()
    try:
        observation = _await_observation(matching)
    finally:
        graph.shut_down()
    if "failure" in observation:
        pytest.fail(f"the probe raised:\n{observation['failure']}")
    return observation


# ---------------------------------------------------------------------------
# Layout
# ---------------------------------------------------------------------------


@processor(execution="manual")
class NumpyViewProbe:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _observe("numpy_view", lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        with ctx.gpu_limited_access.acquire_pixel_buffer(
            SURFACE_WIDTH, SURFACE_HEIGHT
        ) as surface_handle:
            surface_handle.lock(read_only=False)
            view = surface_handle.as_numpy()
            observation = {
                "shape": view.shape,
                "dtype": str(view.dtype),
                "strides": view.strides,
                "bytes_per_row": surface_handle.bytes_per_row,
                "owns_its_data": view.flags["OWNDATA"],
                "dlpack_device": surface_handle.__dlpack_device__(),
            }
            surface_handle.unlock()
            return observation


def test_the_numpy_view_is_a_shared_window_with_the_allocations_row_pitch():
    """`(height, width, 4)` uint8, strides straight off the allocation.

    The strides are the load-bearing part: DLPack counts them in elements and
    numpy in bytes, so a producer that forgot the conversion would hand back an
    array that reads every fourth row as if it were adjacent.
    """
    observation = _run_probe(NumpyViewProbe, "numpy_view")
    assert observation["shape"] == (SURFACE_HEIGHT, SURFACE_WIDTH, 4)
    assert observation["dtype"] == "uint8"
    assert observation["strides"] == (observation["bytes_per_row"], 4, 1)
    assert observation["bytes_per_row"] >= SURFACE_WIDTH * 4
    assert not observation["owns_its_data"], (
        "the view copied the pixels instead of sharing them"
    )
    # kDLCPU — a host-visible mapping, addressed with ordinary loads.
    assert observation["dlpack_device"] == (1, 0)


# ---------------------------------------------------------------------------
# The lock gate
# ---------------------------------------------------------------------------


@processor(execution="manual")
class UnlockedExportProbe:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _observe("unlocked_export", lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        with ctx.gpu_limited_access.acquire_pixel_buffer(
            SURFACE_WIDTH, SURFACE_HEIGHT
        ) as surface_handle:
            outcomes = {}
            try:
                surface_handle.__dlpack__()
                outcomes["unlocked_dlpack"] = "returned a tensor"
            except RuntimeError as refusal:
                outcomes["unlocked_dlpack"] = str(refusal)
            outcomes["unlocked_base_address"] = surface_handle.base_address

            surface_handle.lock()
            outcomes["locked_base_address_is_nonzero"] = surface_handle.base_address != 0
            surface_handle.unlock()

            try:
                surface_handle.as_numpy()
                outcomes["after_unlock"] = "returned a view"
            except RuntimeError as refusal:
                outcomes["after_unlock"] = str(refusal)
            return outcomes


@processor(execution="manual")
class LockModeProbe:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _observe("lock_mode", lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        gpu = ctx.gpu_limited_access
        outcomes = {}
        with gpu.acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT) as surface_handle:
            surface_handle.lock(read_only=True)
            try:
                surface_handle.as_numpy()[0, 0] = [9, 9, 9, 9]
                outcomes["write_under_a_read_lock"] = "succeeded"
            except ValueError as refusal:
                outcomes["write_under_a_read_lock"] = str(refusal)
            surface_handle.unlock()

            surface_handle.lock(read_only=False)
            surface_handle.as_numpy()[0, 0] = [9, 9, 9, 9]
            outcomes["write_under_a_write_lock"] = "succeeded"
            surface_handle.unlock()
        return outcomes


def test_a_read_only_lock_produces_a_read_only_view():
    """`read_only` is carried to the consumer, not just recorded locally.

    Only DLPack's versioned exchange shape has a flags field, so this is
    also the check that the version negotiation is wired up: on the legacy
    shape every tensor arrives read-only and the write lock would look
    broken.
    """
    observation = _run_probe(LockModeProbe, "lock_mode")
    assert "read-only" in observation["write_under_a_read_lock"]
    assert observation["write_under_a_write_lock"] == "succeeded"


def test_an_export_taken_without_a_lock_is_refused():
    """The lock is where the wait for the producer happens.

    Handing out a tensor without one would let a processor read a frame the
    camera is still writing — a torn image that looks like a driver bug rather
    than a missing synchronisation point. `base_address` reports 0 rather than
    a live pointer for the same reason.
    """
    observation = _run_probe(UnlockedExportProbe, "unlocked_export")
    assert "not locked" in observation["unlocked_dlpack"]
    assert observation["unlocked_base_address"] == 0
    assert observation["locked_base_address_is_nonzero"]
    assert "not locked" in observation["after_unlock"], (
        "unlock must close the gate again, not leave it open for the surface's life"
    )


# ---------------------------------------------------------------------------
# Sharing — a write through the view is not a write to a copy
# ---------------------------------------------------------------------------


@processor(execution="manual")
class SharedMemoryProbe:
    """Writes through one handle, reads through a second one resolved by id."""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _observe("shared_memory", lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        gpu = ctx.gpu_limited_access
        with gpu.acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT) as writer_handle:
            writer_handle.lock(read_only=False)
            writer_view = writer_handle.as_numpy()
            writer_view[:, :, :] = 0
            writer_view[3, 5] = [11, 22, 33, 44]
            writer_handle.unlock()

            with gpu.resolve_surface(writer_handle.surface_id) as reader_handle:
                reader_handle.lock()
                reader_view = reader_handle.as_numpy()
                observation = {
                    "pixel_seen_by_the_reader": reader_view[3, 5].tolist(),
                    "an_untouched_pixel": reader_view[0, 0].tolist(),
                }
                reader_handle.unlock()
                return observation


def test_a_write_through_the_view_is_visible_to_another_holder_of_the_surface():
    """Zero-copy means one buffer, not two that agree.

    A second handle resolved by `surface_id` must observe the write, because
    both views address the engine's allocation directly.
    """
    observation = _run_probe(SharedMemoryProbe, "shared_memory")
    assert observation["pixel_seen_by_the_reader"] == [11, 22, 33, 44]
    assert observation["an_untouched_pixel"] == [0, 0, 0, 0]


# ---------------------------------------------------------------------------
# Lifetime — the regression the ticket names
# ---------------------------------------------------------------------------


@processor(execution="manual")
class TensorOutlivesTheSurfaceProbe:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _observe("tensor_outlives_surface", lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        surface_handle = ctx.gpu_limited_access.acquire_pixel_buffer(
            SURFACE_WIDTH, SURFACE_HEIGHT
        )
        surface_handle.lock(read_only=False)
        view = surface_handle.as_numpy()
        view[:, :, :] = 7
        # The frame is done with; Python is not.
        surface_handle.unlock()
        surface_handle.close()
        del surface_handle

        return {
            "still_readable": int(view[1, 1, 0]),
            "sum_after_close": int(view.sum()),
            "expected_sum": SURFACE_WIDTH * SURFACE_HEIGHT * 4 * 7,
        }


def test_a_tensor_outliving_its_surface_keeps_addressing_live_memory():
    """Use-after-free regression.

    A closed handle must not free the mapping a numpy view still points at.
    The engine value and the pool-slot release live behind a refcount every
    outstanding tensor holds a share of, so `close()` drops the handle's share
    and nothing more.

    Mental-revert: move the release back into `close()` — this test reads freed
    memory, and the failure is a segfault or garbage rather than an assertion.
    """
    observation = _run_probe(
        TensorOutlivesTheSurfaceProbe, "tensor_outlives_surface"
    )
    assert observation["still_readable"] == 7
    assert observation["sum_after_close"] == observation["expected_sum"]


@processor(execution="manual")
class PoolCycleProbe:
    """Holds a tensor, then churns the pool past its depth with fresh writes."""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _observe("pool_cycle", lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        gpu = ctx.gpu_limited_access
        held_handle = gpu.acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT)
        held_handle.lock(read_only=False)
        held_view = held_handle.as_numpy()
        held_view[:, :, :] = 3
        held_handle.unlock()
        held_handle.close()

        churn_failures = []
        for cycle in range(8):
            try:
                with gpu.acquire_pixel_buffer(
                    SURFACE_WIDTH, SURFACE_HEIGHT
                ) as churned_handle:
                    churned_handle.lock(read_only=False)
                    # A different value per cycle: if a slot were handed back
                    # while the held tensor still addressed it, this is the
                    # write that would show up there.
                    churned_handle.as_numpy()[:, :, :] = 100 + cycle
                    churned_handle.unlock()
            except RuntimeError as acquire_failure:
                churn_failures.append(str(acquire_failure))

        return {
            "held_values": sorted(set(held_view.flatten().tolist())),
            "churn_failures": churn_failures,
        }


def test_a_held_tensor_is_not_overwritten_by_pool_reuse():
    """Ring-slot reuse is gated on the consumer being done.

    Churning the pool past its depth while a tensor is outstanding must not
    hand that tensor's slot to a new acquire — the held pixels stay as written.
    """
    observation = _run_probe(PoolCycleProbe, "pool_cycle")
    assert observation["churn_failures"] == []
    assert observation["held_values"] == [3], (
        f"the held tensor was overwritten by a later acquire: "
        f"saw {observation['held_values']}"
    )


# ---------------------------------------------------------------------------
# Interop
# ---------------------------------------------------------------------------


@processor(execution="manual")
class DlpackConsumerProbe:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _observe("dlpack_consumer", lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        with ctx.gpu_limited_access.acquire_pixel_buffer(
            SURFACE_WIDTH, SURFACE_HEIGHT
        ) as surface_handle:
            surface_handle.lock(read_only=False)
            surface_handle.as_numpy()[2, 2] = [1, 2, 3, 4]
            # What a framework's own `from_dlpack` does: consume the exporting
            # object, which calls `__dlpack__` on it.
            consumed = numpy.from_dlpack(surface_handle)
            observation = {
                "pixel": consumed[2, 2].tolist(),
                "shape": consumed.shape,
            }
            surface_handle.unlock()
            return observation


def test_a_dlpack_consumer_sees_the_same_pixels():
    observation = _run_probe(DlpackConsumerProbe, "dlpack_consumer")
    assert observation["pixel"] == [1, 2, 3, 4]
    assert observation["shape"] == (SURFACE_HEIGHT, SURFACE_WIDTH, 4)


# ---------------------------------------------------------------------------
# The whole point, end to end
# ---------------------------------------------------------------------------

_effect_reports: "queue.Queue[dict]" = queue.Queue()


@processor
class InvertingEffect:
    """What a user writes: read the frame, change the pixels, pass it on.

    The frame arrives as a surface id, not pixels — so the effect resolves it,
    opens CPU access, and edits the engine's own memory in place.
    """

    @input(delivery_profile="every_sample")
    def video_from_upstream(self) -> None: ...

    def __init__(self) -> None:
        self.frames_seen = 0

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        bag = ctx.inputs.read("video_from_upstream")
        if bag is None or self.frames_seen >= 1:
            return
        self.frames_seen += 1
        frame = VideoFrame.from_bag(bag)
        try:
            with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as surface:
                surface.lock(read_only=False)
                pixels = surface.as_numpy()
                before = pixels[10, 10].tolist()
                # The edit: invert every channel, in place.
                numpy.subtract(255, pixels, out=pixels)
                after_through_this_view = pixels[10, 10].tolist()
                surface.unlock()

            # A second, independent resolve of the same id. If the edit had
            # landed in a copy, this reads the original pattern back.
            with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as reread:
                reread.lock()
                after_through_a_fresh_resolve = reread.as_numpy()[10, 10].tolist()
                reread.unlock()

            _effect_reports.put(
                {
                    "before": before,
                    "after_through_this_view": after_through_this_view,
                    "after_through_a_fresh_resolve": after_through_a_fresh_resolve,
                    "frame_size": (frame.width, frame.height),
                }
            )
        except BaseException:  # noqa: BLE001 — surfaced by the test
            import traceback

            _effect_reports.put({"failure": traceback.format_exc()})


def test_a_processor_edits_a_synthetic_frames_pixels_in_place():
    """The user-facing story: source → effect → the pixels really changed.

    A native source produces frames the interpreter never touches; a Python
    processor reaches into one and rewrites it. Re-resolving the surface
    afterwards is what proves the edit went into the engine's memory rather
    than a copy handed to Python.
    """
    while not _effect_reports.empty():
        _effect_reports.get_nowait()

    graph = RunningGraph()
    pattern = graph.runtime.add(
        TestPatternSource, config={"width": 320, "height": 180}
    )
    effect = graph.runtime.add(InvertingEffect)
    graph.runtime.connect(pattern.output("video"), effect.input("video_from_upstream"))
    graph.start()
    try:
        report = _effect_reports.get(timeout=PIPELINE_TIMEOUT_SECONDS)
    finally:
        graph.shut_down()

    if "failure" in report:
        pytest.fail(f"the effect raised:\n{report['failure']}")
    assert report["frame_size"] == (320, 180)
    assert report["after_through_this_view"] == [255 - channel for channel in report["before"]]
    assert report["after_through_a_fresh_resolve"] == report["after_through_this_view"], (
        "a fresh resolve did not see the edit — the pixels Python wrote were a copy"
    )


@processor(execution="manual")
class UnsupportedFormatProbe:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _observe("unsupported_format", lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        with ctx.gpu_limited_access.acquire_pixel_buffer(
            SURFACE_WIDTH, SURFACE_HEIGHT, "nv12"
        ) as surface_handle:
            surface_handle.lock()
            try:
                surface_handle.__dlpack__()
                outcome = "returned a tensor"
            except ValueError as refusal:
                outcome = str(refusal)
            surface_handle.unlock()
            return {"outcome": outcome}


def test_a_multi_plane_format_is_refused_rather_than_exported_as_luma():
    """NV12 is two planes and DLPack is one buffer.

    Exporting plane 0 would hand back a greyscale image that looks like a
    working colour frame until someone notices the chroma is missing.
    """
    observation = _run_probe(UnsupportedFormatProbe, "unsupported_format")
    assert "one strided linear buffer" in observation["outcome"]
