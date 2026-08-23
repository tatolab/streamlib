# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probes for what a typed cast takes and reaches, against a real producer.

The wheel's Rust tests prove the lease arithmetic against a real surface-share
service with a stand-in surface, and the GPU-free pytest suites prove the
composable's own half against a stand-in capability. What neither can reach is
a real surface published by a real producer and consumed one process away — so
these run against the engine's own sources, in the real placement.

Three families live here, and they need different hardware.

The lagged holders need a camera: only a real capture pool recycles a slot
underneath a held frame. Each holds one delivered frame while the camera runs
ahead of it, then reads that frame's pixels again — the frame read *as a
`VideoFrame`* must come back unchanged, and the same probe reading the bag as
a plain dict must be refused loudly, because the camera recycled the slot and
the published frame id retired with it (#1872). A scene that never moves would
make the typed half vacuous, so scene motion is measured separately.

The host-side bare-protocol probes need only a GPU. They reach a delivered
frame's pixels through the object itself and consume the capsule with plain
numpy, so every part of the seam is real — the resolved handle, the read-only
lock, the engine-minted capsule — while the consumer needs no CUDA build.

The device-side bare-protocol probes need a camera and a CUDA-built consumer.
They are the ones that prove `torch.from_dlpack(frame)` lands on the GPU, and
they keep the camera deliberately: a capture surface is an imported V4L2
DMA-BUF where a test pattern is engine-allocated, so it is the harder subject
for a device export.

Both bare-protocol families run for a cast type the wheel never heard of as
well as for `VideoFrame`, because a protocol that only worked for the shipped
class would be the privilege the plan says it must not have.
"""

import json
import os
import struct
import traceback
import zlib
from dataclasses import dataclass

import numpy

from streamlib import (
    ClaimedSurfacePixelAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,
    log,
    processor,
)

RESULT_MARKER = "MARKER:PROBE_RESULT "


def _the_claim_the_read_took(read: object) -> object:
    """The lease the composable took on the surface a cast object names.

    Read here — rather than inferred from behaviour — so the report says
    whether a claim was taken at all, separately from whether it worked. An
    untyped read hands back a plain bag, which took none by construction.
    """
    reach_the_surface = getattr(read, "pixel_access_to_the_surface_declared_in", None)
    if reach_the_surface is None:
        return None
    return reach_the_surface("surface_id")._check_out_lease_on_the_claimed_surface

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

    # `latest`, not `every_sample`: an unclaimed bag's id is only good for
    # pool-depth frames after publish, so a probe that lets bags queue reads
    # ids the camera has already recycled — refused loudly now (#1872), but
    # that refusal on *arrival* is not what these probes measure. Reading the
    # newest bag keeps arrivals current; the held frame still gets lapped.
    @input(delivery_profile="latest")
    def video_from_upstream(self) -> None: ...

    def __init__(self) -> None:
        self.held_frame: object | None = None
        self.held_surface_id: "str | None" = None
        self.claim_taken: "bool | None" = None
        self.pixels_as_delivered: "numpy.ndarray | None" = None
        self.pixels_of_the_previous_arrival: "numpy.ndarray | None" = None
        self.frames_the_producer_ran_ahead = 0
        self.the_source_produced_a_different_picture = False
        self.arrivals_already_recycled_before_the_probe_read_them = 0
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
            self.claim_taken = _the_claim_the_read_took(read) is not None
            self.pixels_as_delivered = self._sample_pixels(ctx, arrived_surface_id)
            self.pixels_of_the_previous_arrival = self.pixels_as_delivered
            return

        # Does the source actually produce different pictures? Measured on
        # freshly arrived frames, never on the held slot — the held slot is
        # what the result below is about, so reading it here would be the same
        # measurement twice and would confirm nothing. Without this, a still
        # scene makes both probes report "unchanged" and the pair proves
        # nothing at all. An arrival the camera lapped before this probe got
        # to it resolves as a recycled-frame refusal — counted, not fatal:
        # arrival freshness is not what these probes measure.
        try:
            pixels_that_just_arrived = self._sample_pixels(ctx, arrived_surface_id)
        except RuntimeError as refusal:
            if "recycled" not in str(refusal):
                raise
            self.arrivals_already_recycled_before_the_probe_read_them += 1
        else:
            if not (
                pixels_that_just_arrived == self.pixels_of_the_previous_arrival
            ).all():
                self.the_source_produced_a_different_picture = True
            self.pixels_of_the_previous_arrival = pixels_that_just_arrived

        self.frames_the_producer_ran_ahead += 1
        if self.frames_the_producer_ran_ahead < FRAMES_TO_LAG_BY:
            return

        # The held frame, read again after the producer has lapped its pool.
        # An unclaimed frame's slot was recycled, so its published id retired
        # and the resolve is *refused* (#1872) — that refusal is the result,
        # not a probe failure. A claimed frame's slot never recycled, so its
        # id is still current and the pixels must be the delivered ones.
        held_surface_id = self.held_surface_id
        pixels_as_delivered = self.pixels_as_delivered
        assert held_surface_id is not None and pixels_as_delivered is not None
        pixels_now = None
        late_read_refusal = None
        try:
            pixels_now = self._sample_pixels(ctx, held_surface_id)
        except RuntimeError as refusal:
            if "recycled" not in str(refusal):
                raise
            late_read_refusal = str(refusal)
        held_frame_unchanged = pixels_now is not None and bool(
            (pixels_now == pixels_as_delivered).all()
        )

        sample_dir = os.environ.get("STREAMLIB_CAST_CLAIM_SAMPLE_DIR")
        written = []
        if sample_dir:
            os.makedirs(sample_dir, exist_ok=True)
            label = type(self).__name__
            samples = [("delivered", pixels_as_delivered)]
            if pixels_now is not None:
                samples.append(("after_lag", pixels_now))
            for name, pixels in samples:
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
            "late_read_refused_as_recycled": late_read_refusal is not None,
            "late_read_refusal": late_read_refusal,
            "arrivals_already_recycled_before_the_probe_read_them": (
                self.arrivals_already_recycled_before_the_probe_read_them
            ),
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
    is claimed and the camera is free to recycle the slot. The late re-read
    must be refused as recycled; a lookup that *succeeds* here is #1872's
    silent wrongness come back."""

    def _read(self, ctx: RuntimeContextLimitedAccess):
        return ctx.inputs.read("video_from_upstream")


@dataclass(frozen=True, init=False)
class UserAuthoredVideoFrameCast(ClaimedSurfacePixelAccess):
    """A cast type the wheel never heard of, declared the way the change file
    spells it: name the fields, inherit the constructor, get the protocol.

    A source's bag carries keys this type does not declare; they are dropped,
    which is the open-map rule every cast type rides.
    """

    surface_id: str
    width: int
    height: int
    timestamp_ns: int


class _BareProtocolProbe:
    """Reach a delivered frame's pixels through the object itself.

    No resolve, no lock, no context manager — the protocol methods on the thing
    the read handed back. That the same pixels are also reachable the long way
    round is what proves the bare view is *this frame* rather than merely a
    valid tensor. Subclasses differ in how the bag is read and which consumer
    takes the capsule.
    """

    # `latest`: an unclaimed bag's id is only good for pool-depth frames after
    # publish, and this probe reads exactly one frame at whatever moment it
    # starts — a queue of stale bags would refuse on arrival for reasons that
    # have nothing to do with the protocol under test.
    @input(delivery_profile="latest")
    def video_from_upstream(self) -> None: ...

    def __init__(self) -> None:
        self.reported = False

    def _read(self, ctx: RuntimeContextLimitedAccess):
        raise NotImplementedError

    def _observe_the_pixels(self, ctx: RuntimeContextLimitedAccess, frame) -> dict:
        raise NotImplementedError

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if self.reported:
            return
        try:
            frame = self._read(ctx)
        except BaseException:  # noqa: BLE001 — surfaced through the marker line
            self.reported = True
            _report(lambda: {"failure": traceback.format_exc()})
            return
        if frame is None:
            return
        self.reported = True
        _report(lambda: self._observe(ctx, frame))

    def _observe(self, ctx: RuntimeContextLimitedAccess, frame) -> dict:
        return {
            "read_as": type(self).__name__,
            "claim_taken": _the_claim_the_read_took(frame) is not None,
            "surface_id": frame.surface_id,
            # The engine's own answer for which side the bare path serves,
            # readable without any GPU consumer installed.
            "device_the_object_advertised": list(frame.__dlpack_device__()),
            **self._observe_the_pixels(ctx, frame),
        }


class _BareProtocolHostSideProbe(_BareProtocolProbe):
    """The protocol against a real surface, with no CUDA consumer needed.

    `numpy.from_dlpack(frame, device="cpu")` asks the object for the host side
    of the very same surface, so every part of the seam is real — the resolved
    handle, the read-only lock, the capsule — while the consumer is plain
    numpy. What that leaves unproven is only whether a CUDA package can eat the
    device capsule, which is what the torch probes below are for.
    """

    def _observe_the_pixels(self, ctx: RuntimeContextLimitedAccess, frame) -> dict:
        through_the_object = numpy.from_dlpack(frame, device="cpu")
        # The same surface reached the long way round. Same export machinery,
        # so the two are comparable pixel for pixel.
        with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as surface:
            surface.lock()
            through_the_ceremony = numpy.from_dlpack(surface, device="cpu").copy()
            surface.unlock()
        return {
            "host_view_shape": list(through_the_object.shape),
            "host_view_dtype": str(through_the_object.dtype),
            "host_view_shape_through_the_resolve_and_lock": list(
                through_the_ceremony.shape
            ),
            "the_bare_view_is_the_same_pixels": bool(
                (numpy.asarray(through_the_object) == through_the_ceremony).all()
            ),
            "pixels_are_not_all_zero": bool(through_the_ceremony.any()),
        }


class _BareProtocolDeviceSideProbe(_BareProtocolProbe):
    """The device half: a real CUDA package consuming the bare capsule."""

    def _observe_the_pixels(self, ctx: RuntimeContextLimitedAccess, frame) -> dict:
        import torch

        bare_view = torch.from_dlpack(frame)
        observation = {
            "tensor_device": str(bare_view.device),
            "tensor_dtype": str(bare_view.dtype),
            "tensor_shape": list(bare_view.shape),
            "checksum_through_the_bare_view": int(
                bare_view.to(torch.int64).sum().item()
            ),
        }
        with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as surface:
            surface.lock()
            through_the_ceremony = torch.from_dlpack(surface)
            observation["checksum_through_the_resolve_and_lock"] = int(
                through_the_ceremony.to(torch.int64).sum().item()
            )
            observation["tensor_shape_through_the_resolve_and_lock"] = list(
                through_the_ceremony.shape
            )
            surface.unlock()

        sample_dir = os.environ.get("STREAMLIB_CAST_CLAIM_SAMPLE_DIR")
        if sample_dir:
            os.makedirs(sample_dir, exist_ok=True)
            path = os.path.join(sample_dir, f"{type(self).__name__}_bare_dlpack.png")
            _write_png(path, bare_view.cpu().numpy())
            observation["png_samples"] = [path]
        return observation


@processor
class AUserAuthoredCastReachesItsPixelsBareProbe(_BareProtocolHostSideProbe):
    """The no-privilege half: a type the wheel does not ship gets the protocol
    by composing the shipped piece, and nothing else."""

    def _read(self, ctx: RuntimeContextLimitedAccess):
        return ctx.inputs.read("video_from_upstream", into=UserAuthoredVideoFrameCast)


@processor
class TheShippedVideoFrameReachesItsPixelsBareProbe(_BareProtocolHostSideProbe):
    """The parity half: `VideoFrame` is built from that same piece, so it must
    reach its pixels the same way and reach the same pixels."""

    def _read(self, ctx: RuntimeContextLimitedAccess):
        return ctx.inputs.read("video_from_upstream", into=VideoFrame)


@processor
class AUserAuthoredCastReachesItsPixelsAsACudaTensorProbe(
    _BareProtocolDeviceSideProbe
):
    """The no-privilege half with a real CUDA package taking the capsule."""

    def _read(self, ctx: RuntimeContextLimitedAccess):
        return ctx.inputs.read("video_from_upstream", into=UserAuthoredVideoFrameCast)


@processor
class TheShippedVideoFrameReachesItsPixelsAsACudaTensorProbe(
    _BareProtocolDeviceSideProbe
):
    """The parity half on the device side."""

    def _read(self, ctx: RuntimeContextLimitedAccess):
        return ctx.inputs.read("video_from_upstream", into=VideoFrame)


# ---- the write doors: an edit through the object, seen on the surface -------


class _TheEditWentWrong(Exception):
    """Raised inside a write scope on purpose, to leave it the failing way."""


class _WriteDoorProbe(_BareProtocolProbe):
    """Edit a delivered frame through one of its write doors, then look at the
    surface itself again.

    Published means every other holder observes the edit, so the check is a
    *fresh* resolve of the same surface id — the surface's own memory, not the
    view the scope handed out. Editing a few rows rather than the whole frame
    is what separates a published edit from a wholesale overwrite: the rest of
    the frame has to still be the picture the producer sent.
    """

    ROWS_TO_EDIT = 8
    EDIT_VALUE = 7

    def _surface_pixels_now(self, ctx: RuntimeContextLimitedAccess, surface_id: str):
        with ctx.gpu_limited_access.resolve_surface(surface_id) as surface:
            surface.lock()
            pixels = numpy.from_dlpack(surface, device="cpu").copy()
            surface.unlock()
        return pixels

    def _edit_the_top_rows(self, frame) -> None:
        raise NotImplementedError

    def _observe_the_pixels(self, ctx: RuntimeContextLimitedAccess, frame) -> dict:
        before = self._surface_pixels_now(ctx, frame.surface_id)
        self._edit_the_top_rows(frame)
        after = self._surface_pixels_now(ctx, frame.surface_id)
        edited_rows = slice(0, self.ROWS_TO_EDIT)
        the_rest = slice(self.ROWS_TO_EDIT, None)
        return {
            "the_frame_did_not_already_carry_the_edit": bool(
                (before[edited_rows] != self.EDIT_VALUE).any()
            ),
            "the_edited_rows_carry_the_edit": bool(
                (after[edited_rows] == self.EDIT_VALUE).all()
            ),
            "the_rest_of_the_frame_is_untouched": bool(
                (after[the_rest] == before[the_rest]).all()
            ),
        }


@processor
class TheGpuWriteDoorEditsTheFrameProbe(_WriteDoorProbe):
    """`with frame.writable() as t:` — a CUDA package editing a live frame in
    place, with the edit on the surface once the block ends."""

    def _read(self, ctx: RuntimeContextLimitedAccess):
        return ctx.inputs.read("video_from_upstream", into=VideoFrame)

    def _edit_the_top_rows(self, frame) -> None:
        import torch

        with frame.writable() as device_tensor:
            torch.from_dlpack(device_tensor)[: self.ROWS_TO_EDIT, :, :] = self.EDIT_VALUE


@processor
class ARaiseInsideTheGpuWriteDoorDiscardsTheEditProbe(_WriteDoorProbe):
    """The other half of the one write rule: the edit did not finish, so the
    engine keeps the complete frame it already held — and the raise is never
    suppressed on the way out."""

    def _read(self, ctx: RuntimeContextLimitedAccess):
        return ctx.inputs.read("video_from_upstream", into=VideoFrame)

    def _observe_the_pixels(self, ctx: RuntimeContextLimitedAccess, frame) -> dict:
        import torch

        before = self._surface_pixels_now(ctx, frame.surface_id)
        the_exception_propagated = False
        try:
            with frame.writable() as device_tensor:
                torch.from_dlpack(device_tensor)[: self.ROWS_TO_EDIT, :, :] = (
                    self.EDIT_VALUE
                )
                raise _TheEditWentWrong
        except _TheEditWentWrong:
            the_exception_propagated = True
        after = self._surface_pixels_now(ctx, frame.surface_id)
        return {
            "the_exception_propagated": the_exception_propagated,
            "the_surface_still_holds_the_frame_the_producer_sent": bool(
                (after == before).all()
            ),
        }


@processor
class TheCpuWriteDoorEditsTheFrameProbe(_WriteDoorProbe):
    """`with frame.cpu() as img:` — the named slow path, editing a live frame
    through plain numpy with no CUDA consumer anywhere in it."""

    def _read(self, ctx: RuntimeContextLimitedAccess):
        return ctx.inputs.read("video_from_upstream", into=VideoFrame)

    def _edit_the_top_rows(self, frame) -> None:
        with frame.cpu() as host_pixels:
            host_pixels[: self.ROWS_TO_EDIT, :, :] = self.EDIT_VALUE


@processor
class ARaiseInsideTheCpuWriteDoorPropagatesProbe(_WriteDoorProbe):
    """The CPU door's exception path, reported as what it is.

    The host view *is* the surface's own mapping, so bytes already written are
    already in the frame — there is no staging to drop. What the door owes is
    that the raise reaches the caller and the scope still closes, which is what
    this measures; a discard claim here would be a claim the mapping cannot
    keep.
    """

    def _read(self, ctx: RuntimeContextLimitedAccess):
        return ctx.inputs.read("video_from_upstream", into=VideoFrame)

    def _observe_the_pixels(self, ctx: RuntimeContextLimitedAccess, frame) -> dict:
        the_exception_propagated = False
        try:
            with frame.cpu():
                raise _TheEditWentWrong
        except _TheEditWentWrong:
            the_exception_propagated = True
        return {
            "the_exception_propagated": the_exception_propagated,
            "the_door_opens_again_after_a_raise": bool(
                self._surface_pixels_now(ctx, frame.surface_id).any()
            ),
        }
