# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`LowerThirdRundown`: sends the next lower third of a rundown to a `BrowserSource`'s `data` input."""

from tatolab.stream import Output, Records, RuntimeContextLimitedAccess, node

from ..lower_third_template import DEMO_RUNDOWN, LOWER_THIRD_RECORD_NAMES

SECONDS_ON_AIR_PER_LOWER_THIRD = 4


@node.meta(description="Sends the next lower third of the rundown to the page")
@node.process(execution="continuous", interval_ms=SECONDS_ON_AIR_PER_LOWER_THIRD * 1000)
class LowerThirdRundown:
    """Cycles through `DEMO_RUNDOWN`, one `LowerThirdTemplate` per tick."""

    def __init__(self) -> None:
        self._lower_thirds_sent = 0

    lower_third_to_page = Output(
        Records,
        names=LOWER_THIRD_RECORD_NAMES,
        description="One `LowerThirdTemplate` record per message, for the page",
    )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        next_lower_third = DEMO_RUNDOWN[self._lower_thirds_sent % len(DEMO_RUNDOWN)]
        self._lower_thirds_sent += 1
        ctx.outputs.write(self.lower_third_to_page, next_lower_third.as_record())
