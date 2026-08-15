# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probes for the claim a typed cast takes, against a live camera.

The wheel's Rust tests prove the lease arithmetic against a real surface-share
service with a stand-in surface. What they cannot reach is a real producer
recycling a real pool slot underneath a real DMA-BUF surface — so these run on
the rig, against the camera, whose pool recycles a slot every few frames.

Each probe holds one delivered frame while the camera runs ahead of it, then
reads that frame's pixels again. The pair is the point: the frame read *as a
`VideoFrame`* must come back unchanged, and the same probe reading the bag as a
plain dict must not — a scene that never moves would make both vacuous.
"""

import json
import os
import struct
import traceback
import zlib

import numpy

from streamlib import (
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,
    log,
    processor,
)

RESULT_MARKER = "MARKER:PROBE_RESULT "

# The field the shipped frame keeps its claim in. Read here — rather than
# inferred from behaviour — so the report says whether a claim was taken at
# all, separately from whether it worked.
CLAIM_FIELD = "_check_out_lease_on_this_frames_surface"

# Comfortably past the camera pool's depth, so the producer has cycled its
# slots several times over while the first frame is still held.
FRAMES_TO_LAG_BY = 16


def _write_png(path: str, rgba: "numpy.ndarray") -> None:
    """An RGBA8 array as a PNG, in stdlib only.

    The rig's venv carries no image library, and a reviewer looking at whether
    the pixels are a real camera scene should not have to take a checksum's
    word for it."""
    # Refused rather than written wrong: these files are the evidence, and a
    # PNG whose rows do not match its header opens as garbage without ever
    # raising — which would discredit the run rather than fail it.
    if rgba.dtype != numpy.uint8 or rgba.ndim != 3 or rgba.shape[2] < 4:
        raise ValueError(
            f"the PNG writer takes 8-bit RGBA, got dtype={rgba.dtype} shape={rgba.shape}"
        )
    height, width = rgba.shape[0], rgba.shape[1]
    raw = b"".join(b"\x00" + rgba[row, :, :4].tobytes() for row in range(height))

    def chunk(tag: bytes, body: bytes) -> bytes:
        return (
            struct.pack(">I", len(body))
            + tag
            + body
            + struct.pack(">I", zlib.crc32(tag + body) & 0xFFFFFFFF)
        )

    with open(path, "wb") as png:
        png.write(b"\x89PNG\r\n\x1a\n")
        png.write(chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)))
        png.write(chunk(b"IDAT", zlib.compress(raw, 6)))
        png.write(chunk(b"IEND", b""))


def _report(probe_body) -> None:
    try:
        observation = probe_body()
    except BaseException:  # noqa: BLE001 — re-raised by the asserting test
        observation = {"failure": traceback.format_exc()}
    log.info(RESULT_MARKER + json.dumps({"pid": os.getpid(), **observation}))


class _LaggedHolderProbe:
    """Hold the first frame, let the camera run ahead, read it again.

    Subclasses differ in one line — how the bag is read — which is the whole
    variable under test.
    """

    @input(delivery_profile="every_sample")
    def video_from_upstream(self) -> None: ...

    def __init__(self) -> None:
        self.held_frame: object | None = None
        self.held_surface_id: "str | None" = None
        self.claim_taken: "bool | None" = None
        self.pixels_as_delivered: "numpy.ndarray | None" = None
        self.pixels_of_the_previous_arrival: "numpy.ndarray | None" = None
        self.frames_the_producer_ran_ahead = 0
        self.the_source_produced_a_different_picture = False
        self.reported = False

    def _read(self, ctx: RuntimeContextLimitedAccess):
        raise NotImplementedError

    def _sample_pixels(self, ctx: RuntimeContextLimitedAccess, surface_id: str):
        """A copy of the surface's pixels, taken through a handle that is closed
        before returning — so nothing this returns keeps the frame still."""
        with ctx.gpu_limited_access.resolve_surface(surface_id) as surface:
            surface.lock()
            pixels = numpy.from_dlpack(surface, device="cpu").copy()
            surface.unlock()
        return pixels

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if self.reported:
            return
        try:
            self._observe(ctx)
        except BaseException:  # noqa: BLE001 — surfaced through the marker line
            self.reported = True
            self.held_frame = None
            _report(lambda: {"failure": traceback.format_exc()})

    def _observe(self, ctx: RuntimeContextLimitedAccess) -> None:
        read = self._read(ctx)
        if read is None:
            return

        arrived_surface_id = self._read_current_surface_id(read)

        if self.held_frame is None:
            self.held_frame = read
            self.held_surface_id = arrived_surface_id
            self.claim_taken = getattr(read, CLAIM_FIELD, None) is not None
            self.pixels_as_delivered = self._sample_pixels(ctx, arrived_surface_id)
            self.pixels_of_the_previous_arrival = self.pixels_as_delivered
            return

        # Does the source actually produce different pictures? Measured on
        # freshly arrived frames, never on the held slot — the held slot is
        # what the result below is about, so reading it here would be the same
        # measurement twice and would confirm nothing. Without this, a still
        # scene makes both probes report "unchanged" and the pair proves
        # nothing at all.
        pixels_that_just_arrived = self._sample_pixels(ctx, arrived_surface_id)
        if not (pixels_that_just_arrived == self.pixels_of_the_previous_arrival).all():
            self.the_source_produced_a_different_picture = True
        self.pixels_of_the_previous_arrival = pixels_that_just_arrived

        self.frames_the_producer_ran_ahead += 1
        if self.frames_the_producer_ran_ahead < FRAMES_TO_LAG_BY:
            return

        # The held frame, read again after the producer has lapped its pool.
        held_surface_id = self.held_surface_id
        pixels_as_delivered = self.pixels_as_delivered
        assert held_surface_id is not None and pixels_as_delivered is not None
        pixels_now = self._sample_pixels(ctx, held_surface_id)
        held_frame_unchanged = bool((pixels_now == pixels_as_delivered).all())

        sample_dir = os.environ.get("STREAMLIB_CAST_CLAIM_SAMPLE_DIR")
        written = []
        if sample_dir:
            os.makedirs(sample_dir, exist_ok=True)
            label = type(self).__name__
            for name, pixels in (
                ("delivered", pixels_as_delivered),
                ("after_lag", pixels_now),
            ):
                path = os.path.join(sample_dir, f"{label}_{name}.png")
                _write_png(path, pixels)
                written.append(path)

        self.reported = True
        observation = {
            "read_as": type(self).__name__,
            "claim_taken": self.claim_taken,
            "held_surface_id": held_surface_id,
            "frames_the_producer_ran_ahead": self.frames_the_producer_ran_ahead,
            "held_frame_unchanged": held_frame_unchanged,
            "the_source_produced_a_different_picture": (
                self.the_source_produced_a_different_picture
            ),
            "png_samples": written,
        }
        self.held_frame = None
        _report(lambda: observation)

    @staticmethod
    def _read_current_surface_id(read) -> str:
        return read.surface_id if isinstance(read, VideoFrame) else read["surface_id"]


@processor
class TypedCastHoldsItsFrameProbe(_LaggedHolderProbe):
    """The frame is read as a `VideoFrame`, so the cast claims it. Holding the
    object is the only thing keeping the camera off that slot — no handle, no
    view, no context manager."""

    def _read(self, ctx: RuntimeContextLimitedAccess):
        return ctx.inputs.read("video_from_upstream", into=VideoFrame)


@processor
class UntypedReadHoldsNothingProbe(_LaggedHolderProbe):
    """The control. Same probe, same lag, bag read as a plain dict — so nothing
    is claimed and the camera is free to recycle the slot. If this one also
    comes back unchanged, the scene is not moving and the typed probe proves
    nothing."""

    def _read(self, ctx: RuntimeContextLimitedAccess):
        return ctx.inputs.read("video_from_upstream")
