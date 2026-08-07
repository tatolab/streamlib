# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probes that exercise the pixel-exchange surface from where it really runs.

Each probe runs in its own helper process, acquires or resolves a surface
there, and reports what it observed as one `MARKER:PROBE_RESULT` JSON line —
the same child→parent log forwarding every processor's records ride, so the
observation crosses the process boundary without any channel a test invented.
"""

import json
import os
import traceback

import numpy

from streamlib import VideoFrame, input, log, processor

SURFACE_WIDTH = 64
SURFACE_HEIGHT = 32

RESULT_MARKER = "MARKER:PROBE_RESULT "


def _report(probe_body) -> None:
    """One result line per probe, success or failure — the failure carries the
    traceback so the test fails on the cause rather than a missing marker."""
    try:
        observation = probe_body()
    except BaseException:  # noqa: BLE001 — re-raised by the asserting test
        observation = {"failure": traceback.format_exc()}
    log.info(RESULT_MARKER + json.dumps({"pid": os.getpid(), **observation}))


@processor(execution="manual")
class NumpyViewProbe:
    def setup(self, ctx) -> None:
        _report(lambda: self._probe(ctx))

    def _probe(self, ctx) -> dict:
        with ctx.gpu_limited_access.acquire_pixel_buffer(
            SURFACE_WIDTH, SURFACE_HEIGHT
        ) as surface_handle:
            surface_handle.lock(read_only=False)
            view = surface_handle.as_numpy()
            observation = {
                "shape": list(view.shape),
                "dtype": str(view.dtype),
                "strides": list(view.strides),
                "bytes_per_row": surface_handle.bytes_per_row,
                "owns_its_data": bool(view.flags["OWNDATA"]),
            }
            surface_handle.unlock()
            return observation


@processor(execution="manual")
class UnlockedExportProbe:
    def setup(self, ctx) -> None:
        _report(lambda: self._probe(ctx))

    def _probe(self, ctx) -> dict:
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
            outcomes["locked_base_address_is_real"] = (
                surface_handle.base_address is not None
            )
            surface_handle.unlock()

            try:
                surface_handle.as_numpy()
                outcomes["after_unlock"] = "returned a view"
            except RuntimeError as refusal:
                outcomes["after_unlock"] = str(refusal)
            return outcomes


@processor(execution="manual")
class LockModeProbe:
    def setup(self, ctx) -> None:
        _report(lambda: self._probe(ctx))

    def _probe(self, ctx) -> dict:
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


@processor(execution="manual")
class SharedMemoryProbe:
    """Writes through one handle, reads through a second one resolved by id."""

    def setup(self, ctx) -> None:
        _report(lambda: self._probe(ctx))

    def _probe(self, ctx) -> dict:
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


@processor(execution="manual")
class TensorOutlivesTheSurfaceProbe:
    def setup(self, ctx) -> None:
        _report(lambda: self._probe(ctx))

    def _probe(self, ctx) -> dict:
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


@processor(execution="manual")
class PoolCycleProbe:
    """Holds a tensor, then churns the pool past its depth with fresh writes."""

    def setup(self, ctx) -> None:
        _report(lambda: self._probe(ctx))

    def _probe(self, ctx) -> dict:
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


@processor(execution="manual")
class DlpackConsumerProbe:
    def setup(self, ctx) -> None:
        _report(lambda: self._probe(ctx))

    def _probe(self, ctx) -> dict:
        with ctx.gpu_limited_access.acquire_pixel_buffer(
            SURFACE_WIDTH, SURFACE_HEIGHT
        ) as surface_handle:
            surface_handle.lock(read_only=False)
            surface_handle.as_numpy()[2, 2] = [1, 2, 3, 4]
            # What a CPU framework's `from_dlpack` does: consume the exporting
            # object, asking for the host side — a graph frame's natural side
            # is the device, so the request is load-bearing, not decoration.
            consumed = numpy.from_dlpack(surface_handle, device="cpu")
            observation = {
                "pixel": consumed[2, 2].tolist(),
                "shape": list(consumed.shape),
            }
            surface_handle.unlock()
            return observation


@processor(execution="manual")
class UnsupportedFormatProbe:
    def setup(self, ctx) -> None:
        _report(lambda: self._probe(ctx))

    def _probe(self, ctx) -> dict:
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

    def process(self, ctx) -> None:
        bag = ctx.inputs.read("video_from_upstream")
        if bag is None or self.frames_seen >= 1:
            return
        self.frames_seen += 1
        frame = VideoFrame.from_bag(bag)

        def probe() -> dict:
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

            return {
                "before": before,
                "after_through_this_view": after_through_this_view,
                "after_through_a_fresh_resolve": after_through_a_fresh_resolve,
                "frame_size": [frame.width, frame.height],
            }

        _report(probe)
