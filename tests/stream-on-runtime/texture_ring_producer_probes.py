# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A Python node that *produces* texture-backed frames, and one that reads them.

Every other GPU probe in this suite consumes frames a native block produced.
These two close the other direction: a Python source in a node interpreter
allocates its own output ring, writes pixels into a slot from that interpreter, and
publishes the slot's surface id — which another process resolves and reads.

Both report over the same `MARKER:PROBE_RESULT` lines on `tatolabd`'s stderr
the other probes use, tagged with `probe` because a scenario runs two of them.
"""

import json
import os
import traceback
from typing import Annotated, Callable

from tatolab.stream import (
    ColorInfo,
    Input,
    InputPin,
    NodeOutputTextureRing,
    OutputPin,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    TexturePixelFormats,
    TextureToWrite,
    clock,
    log,
    node,
)

FRAME_WIDTH = 64
FRAME_HEIGHT = 32
RING_DEPTH = 2
# The engine's mailbox depth of an `ordered` input port and the default borrow
# quota of every input port: the pool keeps that many more slots unheld for
# the one link out of the ring, so a frame its mailbox queued is never
# recycled under it.
ORDERED_INPUT_PORT_MAILBOX_DEPTH = 16
DEFAULT_INPUT_PORT_BORROW_QUOTA = 2
SLOTS_THE_RING_ROTATES_THROUGH_FOR_ONE_ORDERED_LINK = (
    RING_DEPTH + ORDERED_INPUT_PORT_MAILBOX_DEPTH + DEFAULT_INPUT_PORT_BORROW_QUOTA
)
FRAMES_PUBLISHED_WHILE_THE_FIRST_IS_HELD = (
    SLOTS_THE_RING_ROTATES_THROUGH_FOR_ONE_ORDERED_LINK + RING_DEPTH
)
MINIMUM_INTERVAL_BETWEEN_HELD_FRAME_PUBLISHES_NS = 150_000_000

RESULT_MARKER = "MARKER:PROBE_RESULT "

# `parse_texture_usages` adds COPY_SRC | COPY_DST to every acquire, which is
# what the CPU write door and its read-back need; binding is what makes the
# slot samplable by anything downstream that wants to draw it.
RING_TEXTURE_FORMAT = "rgba8_unorm"
RING_TEXTURE_USAGE = ["texture_binding"]


def _report(probe_name: str, observation_body) -> None:
    """One result line per observation — a failure carries its own traceback,
    which names the cause better than a missing marker does."""
    try:
        observation = observation_body()
    except BaseException:  # noqa: BLE001 — re-raised by the asserting test
        observation = {"failure": traceback.format_exc()}
    log.info(
        RESULT_MARKER
        + json.dumps({"probe": probe_name, "pid": os.getpid(), **observation})
    )


def pixel_value_of_frame(frame_index: int) -> int:
    """The value every channel of every pixel carries in frame `frame_index`.

    Offset off zero so a frame that was never written cannot pass for frame 0.
    """
    return 10 + frame_index


@node(execution="continuous", interval_ms=10)
def TextureRingPublishingVideoSource(
    *,
    frames_to_downstream: OutputPin[Annotated[Texture, TexturePixelFormats(RING_TEXTURE_FORMAT)]],
    frames_to_publish: Input[int] = Input(RING_DEPTH),
    # Paces the source so a consumer in another process takes its claim on
    # the first frame before the ring's depth of publishes has gone by.
    minimum_interval_between_publishes_ns: Input[int] = Input(0),
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Publishes frames from its own output ring, one slot per frame."""
    output_texture_ring = NodeOutputTextureRing(
        RING_TEXTURE_FORMAT, RING_TEXTURE_USAGE, depth=RING_DEPTH
    )
    last_publish_ns: "int | None" = None
    surface_ids_published_so_far: "list[str]" = []

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal last_publish_ns
        frame_index = len(surface_ids_published_so_far)
        if frame_index >= frames_to_publish.get():
            return
        now_ns = clock.monotonic_now_ns()
        if (
            last_publish_ns is not None
            and now_ns - last_publish_ns < minimum_interval_between_publishes_ns.get()
        ):
            return
        last_publish_ns = now_ns

        slot_for_this_frame = output_texture_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, FRAME_WIDTH, FRAME_HEIGHT
        )
        slot_for_this_frame.lock(read_only=False)
        try:
            slot_for_this_frame.as_numpy()[:] = pixel_value_of_frame(frame_index)
        finally:
            slot_for_this_frame.unlock()

        frames_to_downstream.write(
            TextureToWrite.of_texture(slot_for_this_frame, ColorInfo()),
        )
        surface_ids_published_so_far.append(slot_for_this_frame.surface_id)

        if len(surface_ids_published_so_far) == frames_to_publish.get():
            _report(
                "TextureRingPublishingVideoSource",
                lambda: {"surface_ids_published": surface_ids_published_so_far},
            )

    return each_frame


@node(execution="reactive")
def PublishedFramePixelReadingSink(
    *,
    frames_from_upstream: InputPin[Annotated[Texture, StreamDelivery("ordered")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Resolves each published surface id and reports the pixels behind it."""

    frames_read = 0

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal frames_read
        frame = frames_from_upstream.read()
        if frame is None:
            return
        frame_index = frames_read
        frames_read += 1

        def read_the_frames_pixels() -> dict:
            with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as surface:
                surface.lock()
                top_left_pixel = surface.as_numpy()[0, 0].tolist()
                surface.unlock()
            return {
                "frame_index": frame_index,
                "surface_id": frame.surface_id,
                "extent": [frame.width, frame.height],
                "top_left_pixel": top_left_pixel,
            }

        _report("PublishedFramePixelReadingSink", read_the_frames_pixels)

    return each_frame


@node(execution="reactive")
def PublishedFrameIdRecordingSink(
    *,
    frames_from_upstream: InputPin[Annotated[Texture, StreamDelivery("ordered")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Records each published surface id without resolving it — the link an
    output port needs, for a scenario that reads only the producer's report."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        frame = frames_from_upstream.read()
        if frame is None:
            return
        surface_id = frame.surface_id
        _report("PublishedFrameIdRecordingSink", lambda: {"surface_id": surface_id})

    return each_frame


def _top_left_pixel_of(ctx, surface_id: str) -> "list[int]":
    with ctx.gpu_limited_access.resolve_surface(surface_id) as surface:
        surface.lock()
        top_left_pixel = surface.as_numpy()[0, 0].tolist()
        surface.unlock()
    return top_left_pixel


@node(execution="reactive")
def ClaimedFrameHoldingSink(
    *,
    frames_from_upstream: InputPin[Annotated[Texture, StreamDelivery("ordered")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Claims the first frame with a typed read and holds it, re-reading its
    pixels as every later frame arrives."""

    _held_frame: "Texture | None" = None
    later_frames_seen = 0

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal _held_frame, later_frames_seen
        if _held_frame is None:
            _held_frame = frames_from_upstream.read()
            return
        later_frame = frames_from_upstream.read()
        if later_frame is None:
            return
        later_frames_seen += 1
        held_frame = _held_frame
        later_surface_id = later_frame.surface_id

        def reread_the_held_frame() -> dict:
            return {
                "held_surface_id": held_frame.surface_id,
                "later_surface_id": later_surface_id,
                "later_frames_seen": later_frames_seen,
                "held_top_left_pixel": _top_left_pixel_of(ctx, held_frame.surface_id),
            }

        _report("ClaimedFrameHoldingSink", reread_the_held_frame)

    return each_frame


@node(execution="reactive")
def UnclaimedFrameHoldingSink(
    *,
    frames_from_upstream: InputPin[Annotated[Texture, StreamDelivery("ordered")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Keeps only the first frame's id — no claim — and tries it again as every
    later frame arrives: the negative control for the claimed sink."""

    _first_surface_id: "str | None" = None
    later_frames_seen = 0

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal _first_surface_id, later_frames_seen
        frame = frames_from_upstream.read()
        if frame is None:
            return
        if _first_surface_id is None:
            _first_surface_id = frame.surface_id
            return
        later_frames_seen += 1
        first_surface_id = _first_surface_id
        try:
            observation = {
                "held_top_left_pixel": _top_left_pixel_of(ctx, first_surface_id)
            }
        except Exception as refusal:  # noqa: BLE001 — the refusal is the observation
            observation = {"refusal": str(refusal)}
        log.info(
            RESULT_MARKER
            + json.dumps(
                {
                    "probe": "UnclaimedFrameHoldingSink",
                    "pid": os.getpid(),
                    "held_surface_id": first_surface_id,
                    "later_frames_seen": later_frames_seen,
                    **observation,
                }
            )
        )

    return each_frame
