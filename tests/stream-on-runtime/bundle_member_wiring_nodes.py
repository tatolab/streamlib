# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The bundle, the number source and the reporter `test_bundle_member_wiring.py` wires through
`MakeBundle` and `BreakBundle`.

In its own module because a helper process imports each class by its import path, and a class
declared inside a pytest module would have the child import the test suite.
"""

import json
from dataclasses import dataclass

from tatolab.stream import InputPin, OutputPin, log, node

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
