# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The source, the sink and the reporter `test_held_texture_member.py` wires around
`MakeBundle` and `BreakBundle` to show a Texture member held at the read keeps its pixels.

In its own module because a helper process imports each class by its import path, and a class
declared inside a pytest module would have the child import the test suite.
"""

import json
from dataclasses import dataclass
from typing import Annotated, Callable

from tatolab.stream import (
    ColorInfo,
    InputPin,
    NodeOutputTextureRing,
    OutputPin,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    TexturePixelFormats,
    TextureToWrite,
    log,
    node,
)

PICTURE_WIDTH = 64
PICTURE_HEIGHT = 32
PICTURE_FORMAT = "rgba8_unorm"

#: The value every channel of the one picture written into the bundle carries.
HELD_PICTURE_VALUE = 77
#: The value every channel of every later frame from the same ring carries.
CHURN_VALUE = 200

#: The level the source writes into the bundle's `level` member each tick, so the bundle is
#: made again each tick while its picture link stays quiet.
LEVEL_WRITTEN = 0.5

#: How many acquires the source attempts between two reports of its ring, refused or not.
ACQUIRES_PER_RING_REPORT = 50

#: How many bundles the reporter reads off the held picture before it reports.
BUNDLES_TO_READ = 150


@dataclass(frozen=True)
class MeteredPicture:
    """A picture with the level metered off it, which `MakeBundle` makes and `BreakBundle`
    breaks as one Bundle frame."""

    picture: Texture
    level: float


@node(execution="continuous", interval_ms=20)
def PublishesOnePictureThenRefillsItsRing(
    *,
    picture: OutputPin[Annotated[Texture, TexturePixelFormats(PICTURE_FORMAT)]],
    churn: OutputPin[Annotated[Texture, TexturePixelFormats(PICTURE_FORMAT)]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Writes one picture of `HELD_PICTURE_VALUE` to `picture`, then a frame of `CHURN_VALUE`
    from the same ring to `churn` every tick, so the ring turns over while the picture link
    stays quiet."""

    ring = NodeOutputTextureRing(PICTURE_FORMAT, ["texture_binding"], depth=2)
    held_picture_surface_id: str | None = None
    churn_surface_ids: set[str] = set()
    churn_frames_written = 0
    acquires_refused = 0
    acquires_attempted = 0

    def write_the_next_frame(ctx) -> None:
        nonlocal acquires_refused, churn_frames_written, held_picture_surface_id
        try:
            slot = ring.next_texture_for_this_frame(
                ctx.gpu_limited_access, PICTURE_WIDTH, PICTURE_HEIGHT
            )
        except Exception as refusal:  # noqa: BLE001 — a refused acquire is what is counted
            acquires_refused += 1
            log.warning(f"the ring refused a frame: {refusal}")
            return
        value = (
            HELD_PICTURE_VALUE if held_picture_surface_id is None else CHURN_VALUE
        )
        slot.lock(read_only=False)
        try:
            slot.as_numpy()[:] = value
        finally:
            slot.unlock()
        frame = TextureToWrite.of_texture(slot, ColorInfo())
        if held_picture_surface_id is None:
            held_picture_surface_id = slot.surface_id
            picture.write(frame)
            return
        churn.write(frame)
        churn_frames_written += 1
        churn_surface_ids.add(slot.surface_id.split("#")[0])

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal acquires_attempted
        write_the_next_frame(ctx)
        acquires_attempted += 1
        if acquires_attempted % ACQUIRES_PER_RING_REPORT == 0:
            log.info(
                "MARKER:RING_TURNED_OVER "
                + json.dumps(
                    {
                        "held_picture_surface_id": held_picture_surface_id,
                        "acquires_attempted": acquires_attempted,
                        "churn_frames_written": churn_frames_written,
                        "churn_slots": sorted(churn_surface_ids),
                        "acquires_refused": acquires_refused,
                    }
                )
            )

    return each_frame


@node(execution="continuous", interval_ms=20)
def WritesALevelEveryTick(
    *,
    level: OutputPin[float],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Writes `LEVEL_WRITTEN` every tick."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        level.write(LEVEL_WRITTEN)

    return each_frame


@node(execution="reactive")
def LetsEachFrameGo(
    *,
    frames: InputPin[Annotated[Texture, StreamDelivery("newest")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reads each frame it is sent and keeps none."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        frames.read()

    return each_frame


@node(execution="reactive")
def ReportsThePixelsOfEachPictureItReads(
    *,
    picture: InputPin[Annotated[Texture, StreamDelivery("ordered")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reads the pixels of the picture taken off each bundle, and reports once it has read
    `BUNDLES_TO_READ` of them."""

    pictures_read = 0
    pictures_not_held = 0
    values_seen: set[int] = set()
    surface_ids: set[str] = set()
    failures: list[str] = []
    reported = False

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal pictures_not_held, pictures_read, reported, values_seen
        newest_picture = picture.read()
        if newest_picture is None or reported:
            return
        pictures_read += 1
        surface_ids.add(newest_picture.surface_id)
        try:
            with ctx.gpu_limited_access.resolve_surface(newest_picture.surface_id) as surface:
                surface.lock()
                pixels = surface.as_numpy().copy()
                surface.unlock()
            values = {int(value) for value in set(pixels.reshape(-1).tolist())}
            values_seen |= values
            if values != {HELD_PICTURE_VALUE}:
                pictures_not_held += 1
        except Exception as failure:  # noqa: BLE001 — a picture that cannot be read is not held
            pictures_not_held += 1
            if len(failures) < 3:
                failures.append(str(failure))
        if pictures_read >= BUNDLES_TO_READ:
            reported = True
            log.info(
                "MARKER:HELD_PICTURE_READ "
                + json.dumps(
                    {
                        "pictures_read": pictures_read,
                        "pictures_not_held": pictures_not_held,
                        "values_seen": sorted(values_seen),
                        "surface_ids": sorted(surface_ids),
                        "failures": failures,
                    }
                )
            )

    return each_frame
