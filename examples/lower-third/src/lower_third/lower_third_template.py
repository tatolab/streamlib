# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`LowerThirdTemplate`: the record `templates/lower_third.html` renders, and the rundown it cycles through."""

import dataclasses
from typing import Literal

LowerThirdVariant = Literal["live", "guest", "breaking"]
"""The page's template styles; each is a CSS class in `templates/lower_third.html`."""

LOWER_THIRD_VARIANTS: tuple[LowerThirdVariant, ...] = ("live", "guest", "breaking")

LOWER_THIRD_RECORD_NAMES: tuple[str, ...] = ("variant", "title", "subtitle")
"""The keys the page reads from its `tatolab:data` event's `detail`."""


@dataclasses.dataclass(frozen=True)
class LowerThirdTemplate:
    """One lower third: the record the page reads from its `tatolab:data` event's `detail`."""

    variant: LowerThirdVariant
    title: str
    subtitle: str

    def __post_init__(self) -> None:
        if self.variant not in LOWER_THIRD_VARIANTS:
            raise ValueError(
                f"variant {self.variant!r} is not one of the page's templates {LOWER_THIRD_VARIANTS}"
            )

    def as_record(self) -> dict[str, str]:
        """The Records message this lower third is sent to the page as."""
        return {name: getattr(self, name) for name in LOWER_THIRD_RECORD_NAMES}


DEMO_RUNDOWN: tuple[LowerThirdTemplate, ...] = (
    LowerThirdTemplate(variant="live", title="Tatolab Live", subtitle="Camera, browser and Rive in one stream"),
    LowerThirdTemplate(variant="guest", title="Ada Mercer", subtitle="Graphics engineer · on the zero-copy path"),
    LowerThirdTemplate(variant="breaking", title="Template swapped live", subtitle="A data message changed this graphic"),
    LowerThirdTemplate(variant="guest", title="Rui Okafor", subtitle="Producer · driving the rundown"),
)
