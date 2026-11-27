# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`LowerThirdRundown`: sends the next lower third of a rundown to a `BrowserSource`'s `data` input."""

from typing import Annotated, Callable

from tatolab.browser import PageMessage
from tatolab.stream import Description, OutputPin, RuntimeContextLimitedAccess, node

from ..lower_third_template import DEMO_RUNDOWN

SECONDS_ON_AIR_PER_LOWER_THIRD = 4


@node.meta(description="Sends the next lower third of the rundown to the page")
@node(execution="continuous", interval_ms=SECONDS_ON_AIR_PER_LOWER_THIRD * 1000)
def LowerThirdRundown(
    *,
    lower_third_to_page: OutputPin[
        Annotated[
            PageMessage,
            Description("One `LowerThirdTemplate` per message, for the page"),
        ]
    ],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Cycles through `DEMO_RUNDOWN`, one `LowerThirdTemplate` per tick."""

    lower_thirds_sent = 0

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal lower_thirds_sent
        del ctx
        next_lower_third = DEMO_RUNDOWN[lower_thirds_sent % len(DEMO_RUNDOWN)]
        lower_thirds_sent += 1
        lower_third_to_page.write(PageMessage(next_lower_third.as_record()))

    return each_frame
