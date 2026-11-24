# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The source, the sink and the reporter `test_held_texture_member.py` wires around
`MakeBundle` and `BreakBundle` to show a Texture member held at the read keeps its pixels.

In its own module because a helper process imports each class by its import path, and a class
declared inside a pytest module would have the child import the test suite.
"""

import json
from dataclasses import dataclass
from typing import Annotated

from tatolab.stream import (
    ColorInfo,
    ImageFrameToWrite,
    InputPin,
    NodeOutputTextureRing,
    OutputPin,
    StreamDelivery,
    Texture,
    TexturePixelFormats,
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
class PublishesOnePictureThenRefillsItsRing:
    """Writes one picture of `HELD_PICTURE_VALUE` to `picture`, then a frame of `CHURN_VALUE`
    from the same ring to `churn` every tick, so the ring turns over while the picture link
    stays quiet."""

    picture: OutputPin[Annotated[Texture, TexturePixelFormats(PICTURE_FORMAT)]]
    churn: OutputPin[Annotated[Texture, TexturePixelFormats(PICTURE_FORMAT)]]

    def __init__(self) -> None:
        self._ring = NodeOutputTextureRing(PICTURE_FORMAT, ["texture_binding"], depth=2)
        self._held_picture_surface_id: str | None = None
        self._churn_surface_ids: set[str] = set()
        self._churn_frames_written = 0
        self._acquires_refused = 0
        self._acquires_attempted = 0

    def process(self, ctx) -> None:
        self._write_the_next_frame(ctx)
        self._acquires_attempted += 1
        if self._acquires_attempted % ACQUIRES_PER_RING_REPORT == 0:
            log.info(
                "MARKER:RING_TURNED_OVER "
                + json.dumps(
                    {
                        "held_picture_surface_id": self._held_picture_surface_id,
                        "acquires_attempted": self._acquires_attempted,
                        "churn_frames_written": self._churn_frames_written,
                        "churn_slots": sorted(self._churn_surface_ids),
                        "acquires_refused": self._acquires_refused,
                    }
                )
            )

    def _write_the_next_frame(self, ctx) -> None:
        try:
            slot = self._ring.next_texture_for_this_frame(
                ctx.gpu_limited_access, PICTURE_WIDTH, PICTURE_HEIGHT
            )
        except Exception as refusal:  # noqa: BLE001 — a refused acquire is what is counted
            self._acquires_refused += 1
            log.warning(f"the ring refused a frame: {refusal}")
            return
        value = (
            HELD_PICTURE_VALUE if self._held_picture_surface_id is None else CHURN_VALUE
        )
        slot.lock(read_only=False)
        try:
            slot.as_numpy()[:] = value
        finally:
            slot.unlock()
        frame = ImageFrameToWrite.of_texture(slot, ColorInfo())
        if self._held_picture_surface_id is None:
            self._held_picture_surface_id = slot.surface_id
            self.picture.write(frame)
            return
        self.churn.write(frame)
        self._churn_frames_written += 1
        self._churn_surface_ids.add(slot.surface_id.split("#")[0])


@node(execution="continuous", interval_ms=20)
class WritesALevelEveryTick:
    """Writes `LEVEL_WRITTEN` every tick."""

    level: OutputPin[float]

    def process(self, ctx) -> None:
        self.level.write(LEVEL_WRITTEN)


@node
class LetsEachFrameGo:
    """Reads each frame it is sent and keeps none."""

    frames: InputPin[Annotated[Texture, StreamDelivery("newest")]]

    def process(self, ctx) -> None:
        self.frames.read()


@node
class ReportsThePixelsOfEachPictureItReads:
    """Reads the pixels of the picture taken off each bundle, and reports once it has read
    `BUNDLES_TO_READ` of them."""

    picture: InputPin[Annotated[Texture, StreamDelivery("ordered")]]

    def __init__(self) -> None:
        self._pictures_read = 0
        self._pictures_not_held = 0
        self._values_seen: set[int] = set()
        self._surface_ids: set[str] = set()
        self._failures: list[str] = []
        self._reported = False

    def process(self, ctx) -> None:
        picture = self.picture.read()
        if picture is None or self._reported:
            return
        self._pictures_read += 1
        self._surface_ids.add(picture.surface_id)
        try:
            with ctx.gpu_limited_access.resolve_surface(picture.surface_id) as surface:
                surface.lock()
                pixels = surface.as_numpy().copy()
                surface.unlock()
            values = {int(value) for value in set(pixels.reshape(-1).tolist())}
            self._values_seen |= values
            if values != {HELD_PICTURE_VALUE}:
                self._pictures_not_held += 1
        except Exception as failure:  # noqa: BLE001 — a picture that cannot be read is not held
            self._pictures_not_held += 1
            if len(self._failures) < 3:
                self._failures.append(str(failure))
        if self._pictures_read >= BUNDLES_TO_READ:
            self._reported = True
            log.info(
                "MARKER:HELD_PICTURE_READ "
                + json.dumps(
                    {
                        "pictures_read": self._pictures_read,
                        "pictures_not_held": self._pictures_not_held,
                        "values_seen": sorted(self._values_seen),
                        "surface_ids": sorted(self._surface_ids),
                        "failures": self._failures,
                    }
                )
            )
