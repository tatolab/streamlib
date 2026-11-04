# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`LowerThirdRundown`: sends the next lower third of a rundown to a `BrowserSource`'s `data` input."""

from tatolab.stream import RuntimeContextLimitedAccess, node

from ..lower_third_template import DEMO_RUNDOWN

SECONDS_ON_AIR_PER_LOWER_THIRD = 4


@node(
    execution="continuous",
    interval_ms=SECONDS_ON_AIR_PER_LOWER_THIRD * 1000,
    description="Sends the next lower third of the rundown to the page",
)
class LowerThirdRundown:
    """Cycles through `DEMO_RUNDOWN`, one `LowerThirdTemplate` per tick."""

    def __init__(self) -> None:
        self._lower_thirds_sent = 0

    @node.output()
    def lower_third_to_page(self) -> None:
        """`LowerThirdTemplate` records, as msgpack maps for the page."""

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        next_lower_third = DEMO_RUNDOWN[self._lower_thirds_sent % len(DEMO_RUNDOWN)]
        self._lower_thirds_sent += 1
        ctx.outputs.write("lower_third_to_page", next_lower_third.to_bytes())
