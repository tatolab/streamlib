# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`LowerThirdTemplate`: the record `templates/lower_third.html` renders, and the rundown it cycles through."""

import dataclasses
from typing import Literal

from tatolab.stream import MsgpackCast

LowerThirdVariant = Literal["live", "guest", "breaking"]
"""The page's template styles; each is a CSS class in `templates/lower_third.html`."""

LOWER_THIRD_VARIANTS: tuple[LowerThirdVariant, ...] = ("live", "guest", "breaking")


@dataclasses.dataclass(frozen=True)
class LowerThirdTemplate(MsgpackCast):
    """One lower third: the msgpack map the page reads from its `tatolab:data` event's `detail`."""

    variant: LowerThirdVariant
    title: str
    subtitle: str

    def __post_init__(self) -> None:
        if self.variant not in LOWER_THIRD_VARIANTS:
            raise ValueError(
                f"variant {self.variant!r} is not one of the page's templates {LOWER_THIRD_VARIANTS}"
            )


DEMO_RUNDOWN: tuple[LowerThirdTemplate, ...] = (
    LowerThirdTemplate(variant="live", title="Tatolab Live", subtitle="Camera, browser and Rive in one stream"),
    LowerThirdTemplate(variant="guest", title="Ada Mercer", subtitle="Graphics engineer · on the zero-copy path"),
    LowerThirdTemplate(variant="breaking", title="Template swapped live", subtitle="A data message changed this graphic"),
    LowerThirdTemplate(variant="guest", title="Rui Okafor", subtitle="Producer · driving the rundown"),
)
