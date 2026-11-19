# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A String, a Menu, an Asset or a List a function node takes or gives, as its island carries it.

Each travels between nodes as a Records message `{"value": <stored JSON form>}`, a List of
numbers or values of parts as an Array frame the native side reads into that same form, so an
island carries each as that record. A body is handed the value in its Python form, a `str`, an
`Asset` or a `list` of its elements' Python forms, and gives one back, which is checked and
carried as the record again.
"""

from __future__ import annotations

import math
from typing import Any

__all__ = [
    "STORED_VALUE_RECORD_KEY",
    "STORED_VALUE_TYPES",
    "the_record_an_island_carries",
    "the_stored_value_of",
    "the_value_a_body_takes",
]

#: The value types a function node's signature gives a String, a Menu, an Asset and a List.
#: Spelled here, not imported, because the `tatolab-stream` in a project's venv may predate
#: them, and the strings are the wire contract both sides hold.
STORED_VALUE_TYPES = ("string", "menu", "asset", "list_of")

#: The key of the one entry of the record a value of a stored value type travels as.
STORED_VALUE_RECORD_KEY = "value"



def the_value_a_body_takes(pin_type: "dict[str, Any]", stored_value: Any, what_it_is: str) -> Any:
    """`stored_value`, the stored JSON form of a value of `pin_type`, in the Python form a body takes it in.

    A value its type does not take is refused with a `ValueError` naming what the type takes.
    """
    match pin_type:
        case {"type": "boolean"}:
            if not isinstance(stored_value, bool):
                raise _not_taken(what_it_is, stored_value, "true or false")
            return stored_value
        case {"type": "integer"}:
            if isinstance(stored_value, float) and stored_value.is_integer():
                return int(stored_value)
            if isinstance(stored_value, bool) or not isinstance(stored_value, int):
                raise _not_taken(what_it_is, stored_value, "a whole number")
            return stored_value
        case {"type": "float"}:
            if isinstance(stored_value, bool) or not isinstance(stored_value, (int, float)):
                raise _not_taken(what_it_is, stored_value, "a number")
            if not math.isfinite(stored_value):
                raise _not_taken(what_it_is, stored_value, "a finite number")
            return float(stored_value)
        case {"type": "string"}:
            if not isinstance(stored_value, str):
                raise _not_taken(what_it_is, stored_value, "text")
            return stored_value
        case {"type": "menu", "option_names": option_names}:
            if stored_value not in option_names:
                raise _not_taken(
                    what_it_is,
                    stored_value,
                    f"one of {', '.join(f'`{option}`' for option in option_names)}",
                )
            return stored_value
        case {"type": "asset"}:
            from tatolab.stream._pin_types import Asset

            try:
                return Asset.from_stored_value(stored_value)
            except (TypeError, ValueError) as not_an_asset:
                raise ValueError(f"{what_it_is}: {not_an_asset}") from not_an_asset
        case {"type": "list_of", "element_type": element_type}:
            if not isinstance(stored_value, list):
                raise _not_taken(what_it_is, stored_value, "a list")
            return [
                the_value_a_body_takes(element_type, element, f"{what_it_is}'s element {index}")
                for index, element in enumerate(stored_value)  # pyright: ignore[reportUnknownArgumentType, reportUnknownVariableType]
            ]
        case _:
            try:
                return _the_value_class_of_parts_of(pin_type).from_stored_value(stored_value)
            except (TypeError, ValueError) as not_of_its_type:
                raise ValueError(f"{what_it_is}: {not_of_its_type}") from not_of_its_type


def the_stored_value_of(pin_type: "dict[str, Any]", value: Any, what_it_is: str) -> Any:
    """`value`, in the Python form a body gives a value of `pin_type`, as its stored JSON form.

    A value of another type is refused with a `TypeError` or `ValueError` naming what it is.
    """
    match pin_type:
        case {"type": "boolean" | "integer" | "float" | "string" | "menu"}:
            return the_value_a_body_takes(pin_type, value, what_it_is)
        case {"type": "asset"}:
            from tatolab.stream._pin_types import Asset

            if type(value) is not Asset:
                raise TypeError(f"{what_it_is} is an `Asset`, and the body gave {type(value).__name__}")
            return value.stored_value()
        case {"type": "list_of", "element_type": element_type}:
            if not isinstance(value, (list, tuple)):
                raise TypeError(f"{what_it_is} is a list, and the body gave {type(value).__name__}")
            return [
                the_stored_value_of(element_type, element, f"{what_it_is}'s element {index}")
                for index, element in enumerate(value)  # pyright: ignore[reportUnknownArgumentType, reportUnknownVariableType]
            ]
        case _:
            value_class = _the_value_class_of_parts_of(pin_type)
            if type(value) is not value_class:
                raise TypeError(
                    f"{what_it_is} is a `{value_class.__name__}`, and the body gave {type(value).__name__}"
                )
            return value.stored_value()


def the_record_an_island_carries(pin_type: "dict[str, Any]", stored_value: Any, what_it_is: str) -> "dict[str, Any]":
    """`stored_value`, checked as a value of `pin_type`, as the record an island carries it as."""
    return {
        STORED_VALUE_RECORD_KEY: the_stored_value_of(
            pin_type, the_value_a_body_takes(pin_type, stored_value, what_it_is), what_it_is
        )
    }


def _the_value_class_of_parts_of(pin_type: "dict[str, Any]") -> Any:
    """The `tatolab.stream` value class of `pin_type`, a Color, a Vector, a Rectangle or a Transform.

    Imported here, not at the top, because the `tatolab-stream` in a project's venv may predate
    the value classes; only a node that declares one names its type.
    """
    from tatolab.stream._pin_types import PIN_VALUE_CLASSES

    for value_class in PIN_VALUE_CLASSES:
        if value_class.pin_type == pin_type:
            return value_class
    raise ValueError(f"a {pin_type!r} value has no Python form a function node takes")


def _not_taken(what_it_is: str, value: Any, takes: str) -> ValueError:
    return ValueError(f"{what_it_is} is {value!r}, and it takes {takes}")
