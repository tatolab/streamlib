# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A pin's annotation classes by the contract key each states, and each annotation as the
contract carries it."""

from __future__ import annotations

from typing import Any, Final

from ._contract_pin_types import PinAnnotationName, PinTypeDescription
from ._number_range import NumberRange
from ._pin_annotations import (
    FrameRate,
    FrameRateFromSetting,
    NumberStep,
    NumberUnit,
    PinControl,
    PinMeaning,
)

__all__ = [
    "ANNOTATION_NAME_OF_EACH_CLASS",
    "annotation_classes_named",
    "annotation_on_the_wire",
]

# Each annotation class, by the contract key it states.
ANNOTATION_NAME_OF_EACH_CLASS: Final[dict[type, PinAnnotationName]] = {
    NumberRange: "range",
    NumberStep: "step",
    NumberUnit: "unit",
    PinMeaning: "meaning",
    PinControl: "control",
    FrameRate: "frame_rate",
    FrameRateFromSetting: "frame_rate",
}


def annotation_on_the_wire(annotation: object) -> Any:
    match annotation:
        case NumberRange():
            return {
                bound: value
                for bound, value in (("minimum", annotation.minimum), ("maximum", annotation.maximum))
                if value is not None
            }
        case NumberStep():
            return annotation.step
        case FrameRate():
            return {
                "stated": {"numerator": annotation.numerator, "denominator": annotation.denominator}
            }
        case FrameRateFromSetting():
            return {"from_config": {"config_key": annotation.setting}}
        case NumberUnit() | PinMeaning() | PinControl():
            return annotation.text
        case _:
            raise AssertionError(f"{annotation!r} is not a pin annotation")


def annotation_classes_named(row: PinTypeDescription) -> str:
    names = [
        f"`{annotation_class.__name__}`"
        for annotation_class, annotation_name in ANNOTATION_NAME_OF_EACH_CLASS.items()
        if annotation_name in row.annotations
    ]
    return ", ".join(names) if names else "none of them"
