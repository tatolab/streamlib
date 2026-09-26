# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Python processor that *produces* texture-backed frames, and one that reads them.

Every other GPU probe in this suite consumes frames a native block produced.
These two close the other direction: a helper-placed Python source allocates
its own output ring, writes pixels into a slot from its child interpreter, and
publishes the slot's surface id — which another process resolves and reads.

Both report over the same `MARKER:PROBE_RESULT` child-to-parent log forwarding
the other probes use, tagged with `probe` because a scenario runs two of them.
"""

import dataclasses
import json
import os
import traceback

from streamlib import (
    ProcessorOutputTextureRing,
    VideoFrame,
    clock,
    input,
    log,
    output,
    processor,
)

FRAME_WIDTH = 64
FRAME_HEIGHT = 32
RING_DEPTH = 2
FRAMES_PUBLISHED_WHILE_THE_FIRST_IS_HELD = 3 * RING_DEPTH
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


@dataclasses.dataclass
class TextureRingPublishingVideoSourceConfig:
    frames_to_publish: int = RING_DEPTH
    # Paces the source so a consumer in another process takes its claim on
    # the first frame before the ring's depth of publishes has gone by.
    minimum_interval_between_publishes_ns: int = 0


@processor(execution="continuous", interval_ms=10)
class TextureRingPublishingVideoSource:
    """Publishes frames from its own output ring, one slot per frame."""

    @output()
    def frames_to_downstream(self) -> None: ...

    def __init__(self, config: TextureRingPublishingVideoSourceConfig) -> None:
        self._output_texture_ring = ProcessorOutputTextureRing(
            RING_TEXTURE_FORMAT, RING_TEXTURE_USAGE, depth=RING_DEPTH
        )
        self._frames_to_publish = config.frames_to_publish
        self._minimum_interval_between_publishes_ns = (
            config.minimum_interval_between_publishes_ns
        )
        self._last_publish_ns: "int | None" = None
        self._surface_ids_published_so_far: "list[str]" = []

    def process(self, ctx) -> None:
        frame_index = len(self._surface_ids_published_so_far)
        if frame_index >= self._frames_to_publish:
            return
        now_ns = clock.monotonic_now_ns()
        if (
            self._last_publish_ns is not None
            and now_ns - self._last_publish_ns
            < self._minimum_interval_between_publishes_ns
        ):
            return
        self._last_publish_ns = now_ns

        slot_for_this_frame = self._output_texture_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, FRAME_WIDTH, FRAME_HEIGHT
        )
        slot_for_this_frame.lock(read_only=False)
        try:
            slot_for_this_frame.as_numpy()[:] = pixel_value_of_frame(frame_index)
        finally:
            slot_for_this_frame.unlock()

        ctx.outputs.write(
            "frames_to_downstream",
            {
                "surface_id": slot_for_this_frame.surface_id,
                "width": FRAME_WIDTH,
                "height": FRAME_HEIGHT,
                "timestamp_ns": clock.monotonic_now_ns(),
                "frame_index": frame_index,
            },
        )
        self._surface_ids_published_so_far.append(slot_for_this_frame.surface_id)

        if len(self._surface_ids_published_so_far) == self._frames_to_publish:
            _report(
                "TextureRingPublishingVideoSource",
                lambda: {"surface_ids_published": self._surface_ids_published_so_far},
            )


@processor
class PublishedFramePixelReadingSink:
    """Resolves each published surface id and reports the pixels behind it."""

    @input(delivery_profile="ordered")
    def frames_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        bag = ctx.inputs.read("frames_from_upstream")
        if bag is None:
            return
        frame = VideoFrame.from_bag(bag)

        def read_the_frames_pixels() -> dict:
            with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as surface:
                surface.lock()
                top_left_pixel = surface.as_numpy()[0, 0].tolist()
                surface.unlock()
            return {
                "frame_index": bag["frame_index"],
                "surface_id": frame.surface_id,
                "extent": [frame.width, frame.height],
                "top_left_pixel": top_left_pixel,
            }

        _report("PublishedFramePixelReadingSink", read_the_frames_pixels)


@processor
class PublishedFrameIdRecordingSink:
    """Records each published surface id without resolving it — the link an
    output port needs, for a scenario that reads only the producer's report."""

    @input(delivery_profile="ordered")
    def frames_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        bag = ctx.inputs.read("frames_from_upstream")
        if bag is None:
            return
        _report("PublishedFrameIdRecordingSink", lambda: {"surface_id": bag["surface_id"]})


def _top_left_pixel_of(ctx, surface_id: str) -> "list[int]":
    with ctx.gpu_limited_access.resolve_surface(surface_id) as surface:
        surface.lock()
        top_left_pixel = surface.as_numpy()[0, 0].tolist()
        surface.unlock()
    return top_left_pixel


@processor
class ClaimedFrameHoldingSink:
    """Claims the first frame with a typed read and holds it, re-reading its
    pixels as every later frame arrives."""

    @input(delivery_profile="ordered")
    def frames_from_upstream(self) -> None: ...

    def __init__(self) -> None:
        self._held_frame: "VideoFrame | None" = None
        self._later_frames_seen = 0

    def process(self, ctx) -> None:
        if self._held_frame is None:
            self._held_frame = ctx.inputs.read("frames_from_upstream", into=VideoFrame)
            return
        bag = ctx.inputs.read("frames_from_upstream")
        if bag is None:
            return
        self._later_frames_seen += 1
        held_frame = self._held_frame

        def reread_the_held_frame() -> dict:
            return {
                "held_surface_id": held_frame.surface_id,
                "later_surface_id": bag["surface_id"],
                "later_frames_seen": self._later_frames_seen,
                "held_top_left_pixel": _top_left_pixel_of(ctx, held_frame.surface_id),
            }

        _report("ClaimedFrameHoldingSink", reread_the_held_frame)


@processor
class UnclaimedFrameHoldingSink:
    """Keeps only the first frame's id — no claim — and tries it again as every
    later frame arrives: the negative control for the claimed sink."""

    @input(delivery_profile="ordered")
    def frames_from_upstream(self) -> None: ...

    def __init__(self) -> None:
        self._first_surface_id: "str | None" = None
        self._later_frames_seen = 0

    def process(self, ctx) -> None:
        bag = ctx.inputs.read("frames_from_upstream")
        if bag is None:
            return
        if self._first_surface_id is None:
            self._first_surface_id = bag["surface_id"]
            return
        self._later_frames_seen += 1
        first_surface_id = self._first_surface_id
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
                    "later_frames_seen": self._later_frames_seen,
                    **observation,
                }
            )
        )
