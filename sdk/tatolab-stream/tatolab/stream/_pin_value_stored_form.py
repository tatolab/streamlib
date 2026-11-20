# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A pin's value in the stored JSON form a pin of its type holds it in, or refused with why."""

from __future__ import annotations

import math
from typing import Final, Literal, Optional

from ._contract_pin_types import (
    BUILT_IN_PIN_TYPES,
    BuiltInPinTypeName,
    PinAnnotations,
    PinType,
    PinTypeDescription,
)
from ._pin_type_choice import PinTypeChoice
from ._pin_types import INTEGER_BOUNDS_OF_EACH_SIZE, PIN_VALUE_CLASSES, Asset, PinValueOfParts
from ._port_family_declaration import PortFamily

__all__ = [
    "PIN_TYPE_DESCRIPTION_BY_NAME",
    "PinValueNotTaken",
    "PinValueNotTakenKind",
    "stored_form_of_a_pin_value",
]

PIN_TYPE_DESCRIPTION_BY_NAME: Final[dict[BuiltInPinTypeName, PinTypeDescription]] = {
    row.name: row for row in BUILT_IN_PIN_TYPES
}
"""Each built-in pin type's row, by its name."""


type PinValueNotTakenKind = Literal[
    "not_of_its_type",
    "outside_its_exact_size",
    "not_finite",
    "not_an_option",
    "outside_its_range",
]
"""Why a pin does not hold a value."""


class PinValueNotTaken(ValueError):
    """A value a pin of its type does not hold: the value refused, and why, worded to follow it."""

    def __init__(self, value: object, why: str, *, kind: PinValueNotTakenKind, pin_type: PinType) -> None:
        super().__init__(f"{value!r}, {why}")
        self.value: object = value
        """The value refused: the whole value, or the element of a List that was refused."""
        self.why: str = why
        """Why, as a clause that follows the value, such as `which is not a Float`."""
        self.kind: PinValueNotTakenKind = kind
        """What kind of refusal it is."""
        self.pin_type: PinType = pin_type
        """The type the refused value was judged as: the pin's, or a List's element type."""


def stored_form_of_a_pin_value(
    value: object, pin_type: PinType, annotations: Optional[PinAnnotations] = None
) -> object:
    """`value` in the stored JSON form a pin of `pin_type` holds it in, inside the range
    `annotations` state; refused with `PinValueNotTaken` when the pin does not hold it."""
    stored = _stored_form_of_its_type(value, pin_type)
    number_range = (annotations or {}).get("range")
    if number_range is not None and isinstance(stored, (int, float)) and not isinstance(stored, bool):
        minimum, maximum = number_range.get("minimum"), number_range.get("maximum")
        if (minimum is not None and stored < minimum) or (maximum is not None and stored > maximum):
            raise PinValueNotTaken(
                stored,
                f"outside its `NumberRange(minimum={minimum!r}, maximum={maximum!r})`",
                kind="outside_its_range",
                pin_type=pin_type,
            )
    return stored


def _stored_form_of_its_type(value: object, pin_type: PinType) -> object:
    row = PIN_TYPE_DESCRIPTION_BY_NAME[pin_type["type"]]
    not_of_its_type = PinValueNotTaken(
        value, f"which is not a {row.label}", kind="not_of_its_type", pin_type=pin_type
    )
    match pin_type:
        case {"type": "boolean"}:
            if not isinstance(value, bool):
                raise not_of_its_type
            return value
        case {"type": "integer"}:
            if isinstance(value, bool) or not isinstance(value, int):
                raise not_of_its_type
            exact_size = pin_type.get("exact_size")
            if exact_size is not None:
                lowest, highest = INTEGER_BOUNDS_OF_EACH_SIZE[exact_size]
                if not lowest <= value <= highest:
                    raise PinValueNotTaken(
                        value,
                        f"outside the {lowest} to {highest} its exact size `{exact_size}` holds",
                        kind="outside_its_exact_size",
                        pin_type=pin_type,
                    )
            return value
        case {"type": "float"}:
            if isinstance(value, bool) or not isinstance(value, (int, float)):
                raise not_of_its_type
            if not math.isfinite(value):
                raise PinValueNotTaken(
                    value, "and a stored number is finite", kind="not_finite", pin_type=pin_type
                )
            return float(value)
        case {"type": "color" | "vector" | "rectangle" | "transform"}:
            value_class = _the_value_class_of(pin_type)
            if type(value) is not value_class:
                raise not_of_its_type
            return value.stored_value()
        case {"type": "string"}:
            if not isinstance(value, str):
                raise not_of_its_type
            return value
        case {"type": "menu", "option_names": option_names}:
            if isinstance(value, PortFamily):
                # A family is written by its name, as a Menu of the family names holds it.
                value = value._wire_name  # pyright: ignore[reportPrivateUsage]
            if value not in option_names:
                raise PinValueNotTaken(
                    value,
                    f"which is none of its options, "
                    f"{', '.join(repr(option) for option in option_names)}",
                    kind="not_an_option",
                    pin_type=pin_type,
                )
            return value
        case {"type": "asset"}:
            if type(value) is not Asset:
                raise not_of_its_type
            return value.stored_value()
        case {"type": "list_of", "element_type": element_type}:
            if not isinstance(value, (list, tuple)):
                raise not_of_its_type
            return [
                _stored_form_of_its_type(element, element_type)
                for element in value  # pyright: ignore[reportUnknownVariableType]
            ]
        case {"type": "pin_type_choice"}:
            if not isinstance(value, PinTypeChoice):
                raise not_of_its_type
            among = pin_type.get("among", [])
            if among and value.chosen not in among:
                raise PinValueNotTaken(
                    value,
                    f"which chooses none of the types it may choose, "
                    f"{', '.join(repr(choosable) for choosable in among)}",
                    kind="not_an_option",
                    pin_type=pin_type,
                )
            return value.chosen
        case _:
            raise not_of_its_type


def _the_value_class_of(pin_type: PinType) -> type[PinValueOfParts]:
    (value_class,) = [
        value_class for value_class in PIN_VALUE_CLASSES if value_class.pin_type == pin_type
    ]
    return value_class
