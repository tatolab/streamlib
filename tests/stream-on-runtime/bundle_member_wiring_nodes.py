# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The bundles, the number source and the reporters `test_bundle_member_wiring.py` wires through
`MakeBundle` and `BreakBundle`.

In its own module because a helper process imports each class by its import path, and a class
declared inside a pytest module would have the child import the test suite.
"""

import json
from dataclasses import dataclass
from typing import Annotated, Callable

from tatolab.stream import InputPin, OutputPin, RuntimeContextLimitedAccess, StreamDelivery, Texture, log, node

#: The level the source writes into the bundle's `level` member.
LEVEL_WRITTEN = 0.25


@dataclass(frozen=True)
class Reading:
    """The bundle `MakeBundle` makes and `BreakBundle` breaks."""

    level: float
    label: str = "unnamed"


@node(execution="continuous", interval_ms=20)
def WritesOneLevelSource(
    *,
    level: OutputPin[float],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Writes `LEVEL_WRITTEN` every tick."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        level.write(LEVEL_WRITTEN)

    return each_frame


@node(execution="continuous", interval_ms=20)
def ReportsTheMembersItReads(
    *,
    level: InputPin[float],
    label: InputPin[str],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reports the level and the label it reads, once both have arrived."""

    latest: "dict[str, object]" = {}
    reported = False

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal reported
        for name, pin in (("level", level), ("label", label)):
            value = pin.read()
            if value is not None:
                latest[name] = value
        if not reported and len(latest) == 2:
            reported = True
            log.info("MARKER:BUNDLE_MEMBERS_READ " + json.dumps(latest))

    return each_frame


@dataclass(frozen=True)
class MeteredPicture:
    """A picture with the level metered off it, which `MakeBundle` makes and `BreakBundle`
    breaks as one Bundle frame."""

    picture: Texture
    level: float


@node(execution="continuous", interval_ms=20)
def ReportsThePictureAndTheLevelItReads(
    *,
    picture: InputPin[Annotated[Texture, StreamDelivery("newest")]],
    level: InputPin[float],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reports the size of the picture and the level it reads, once both have arrived."""

    latest: "dict[str, object]" = {}
    reported = False

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal reported
        newest_picture = picture.read()
        if newest_picture is not None:
            latest["picture"] = [newest_picture.width, newest_picture.height]
        newest_level = level.read()
        if newest_level is not None:
            latest["level"] = newest_level
        if not reported and len(latest) == 2:
            reported = True
            log.info("MARKER:METERED_PICTURE_MEMBERS_READ " + json.dumps(latest))

    return each_frame
