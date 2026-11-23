# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The bundles, the number source and the reporters `test_bundle_member_wiring.py` wires through
`MakeBundle` and `BreakBundle`.

In its own module because a helper process imports each class by its import path, and a class
declared inside a pytest module would have the child import the test suite.
"""

import json
from dataclasses import dataclass
from typing import Annotated

from tatolab.stream import InputPin, OutputPin, StreamDelivery, Texture, log, node

#: The level the source writes into the bundle's `level` member.
LEVEL_WRITTEN = 0.25


@dataclass(frozen=True)
class Reading:
    """The bundle `MakeBundle` makes and `BreakBundle` breaks."""

    level: float
    label: str = "unnamed"


@node(execution="continuous", interval_ms=20)
class WritesOneLevelSource:
    """Writes `LEVEL_WRITTEN` every tick."""

    level: OutputPin[float]

    def process(self, ctx) -> None:
        self.level.write(LEVEL_WRITTEN)


@node(execution="continuous", interval_ms=20)
class ReportsTheMembersItReads:
    """Reports the level and the label it reads, once both have arrived."""

    level: InputPin[float]
    label: InputPin[str]

    def __init__(self) -> None:
        self._latest: "dict[str, object]" = {}
        self._reported = False

    def process(self, ctx) -> None:
        for name, pin in (("level", self.level), ("label", self.label)):
            value = pin.read()
            if value is not None:
                self._latest[name] = value
        if not self._reported and len(self._latest) == 2:
            self._reported = True
            log.info("MARKER:BUNDLE_MEMBERS_READ " + json.dumps(self._latest))


@dataclass(frozen=True)
class MeteredPicture:
    """A picture with the level metered off it, which `MakeBundle` makes and `BreakBundle`
    breaks as one Bundle frame."""

    picture: Texture
    level: float


@node(execution="continuous", interval_ms=20)
class ReportsThePictureAndTheLevelItReads:
    """Reports the size of the picture and the level it reads, once both have arrived."""

    picture: InputPin[Annotated[Texture, StreamDelivery("newest")]]
    level: InputPin[float]

    def __init__(self) -> None:
        self._latest: "dict[str, object]" = {}
        self._reported = False

    def process(self, ctx) -> None:
        picture = self.picture.read()
        if picture is not None:
            self._latest["picture"] = [picture.width, picture.height]
        level = self.level.read()
        if level is not None:
            self._latest["level"] = level
        if not self._reported and len(self._latest) == 2:
            self._reported = True
            log.info("MARKER:METERED_PICTURE_MEMBERS_READ " + json.dumps(self._latest))
