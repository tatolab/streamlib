# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A String, a Menu, an Asset, a List or a bundle a function node takes or gives, as its island carries it.

Each travels between nodes as a Records message `{"value": <stored JSON form>}`, a List of
numbers or values of parts as an Array frame and a bundle as a Records message of its members
by name, each of which the native side reads into that same form, so an island carries each as
that record. A body is handed the value in its Python form, a `str`, an `Asset`, a `list` of its
elements' Python forms or the frozen dataclass a bundle is written as, built straight from its
members, and gives one back, which is checked and carried as the record again.
"""

from __future__ import annotations

import dataclasses
import math
from typing import Any, Optional

__all__ = [
    "STORED_VALUE_RECORD_KEY",
    "STORED_VALUE_TYPES",
    "the_record_an_island_carries",
    "the_stored_value_of",
    "the_value_a_body_takes",
]

#: The value types a function node's signature gives a String, a Menu, an Asset, a List and a
#: bundle. Spelled here, not imported, because the `tatolab-stream` in a project's venv may
#: predate them, and the strings are the wire contract both sides hold.
STORED_VALUE_TYPES = ("string", "menu", "asset", "list_of", "bundle")

#: The key of the one entry of the record a value of a stored value type travels as.
STORED_VALUE_RECORD_KEY = "value"



def the_value_a_body_takes(
    pin_type: "dict[str, Any]",
    stored_value: Any,
    what_it_is: str,
    bundle_class: Optional[type] = None,
) -> Any:
    """`stored_value`, the stored JSON form of a value of `pin_type`, in the Python form a body takes it in.

    A bundle, or each bundle element of a List, is built as `bundle_class`, the dataclass it is
    written as, each member it leaves out given its default. A value its type does not take is
    refused with a `ValueError` naming what the type takes: a bundle that holds a key that is no
    member, or leaves out one with no default, names it.
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
                the_value_a_body_takes(
                    element_type, element, f"{what_it_is}'s element {index}", bundle_class
                )
                for index, element in enumerate(stored_value)  # pyright: ignore[reportUnknownArgumentType, reportUnknownVariableType]
            ]
        case {"type": "bundle", "members": members}:
            if bundle_class is None:
                raise ValueError(f"{what_it_is} is a bundle, and names no dataclass to build it as")
            return bundle_class(**_the_members_a_body_takes(members, stored_value, what_it_is))
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
        case {"type": "bundle", "members": members}:
            member_names = [member["name"] for member in members]
            if not dataclasses.is_dataclass(value) or isinstance(value, type):
                raise TypeError(
                    f"{what_it_is} is a bundle of {', '.join(f'`{name}`' for name in member_names)}, "
                    f"and the body gave {type(value).__name__}"
                )
            field_names = {field.name for field in dataclasses.fields(value)}
            if field_names != set(member_names):
                raise ValueError(
                    f"{what_it_is} is a bundle of {', '.join(f'`{name}`' for name in member_names)}, "
                    f"and the body gave a `{type(value).__name__}` of "
                    f"{', '.join(f'`{name}`' for name in sorted(field_names))}"
                )
            return {
                member["name"]: the_stored_value_of(
                    member["member_type"],
                    getattr(value, member["name"]),
                    f"{what_it_is}'s member `{member['name']}`",
                )
                for member in members
            }
        case _:
            value_class = _the_value_class_of_parts_of(pin_type)
            if type(value) is not value_class:
                raise TypeError(
                    f"{what_it_is} is a `{value_class.__name__}`, and the body gave {type(value).__name__}"
                )
            return value.stored_value()


def the_record_an_island_carries(pin_type: "dict[str, Any]", stored_value: Any, what_it_is: str) -> "dict[str, Any]":
    """`stored_value`, checked as a value of `pin_type`, as the record an island carries it as,
    each bundle in it made whole."""
    return {
        STORED_VALUE_RECORD_KEY: the_stored_value_of(
            pin_type,
            the_value_a_body_takes(
                pin_type, stored_value, what_it_is, _a_dataclass_of_the_bundle_in(pin_type)
            ),
            what_it_is,
        )
    }


def _a_dataclass_of_the_bundle_in(pin_type: "dict[str, Any]") -> Optional[type]:
    """A frozen dataclass of the members of `pin_type`'s bundle, or its List's bundle elements,
    which checks a value as its own dataclass would; `None` for a type that holds no bundle."""
    if pin_type["type"] == "list_of":
        pin_type = pin_type["element_type"]
    if pin_type["type"] != "bundle":
        return None
    return dataclasses.make_dataclass(
        "Bundle", [member["name"] for member in pin_type["members"]], frozen=True
    )


def _the_members_a_body_takes(
    members: "list[dict[str, Any]]", stored_value: Any, what_it_is: str
) -> "dict[str, Any]":
    """Each member of `stored_value`, a bundle's stored JSON form, in its Python form by name,
    a member it leaves out given its default."""
    if not isinstance(stored_value, dict):
        raise _not_taken(what_it_is, stored_value, "an object of its members")
    member_names = [member["name"] for member in members]
    not_members = sorted(key for key in stored_value if key not in member_names)  # pyright: ignore[reportUnknownVariableType]
    if not_members:
        raise ValueError(
            f"{what_it_is} holds {', '.join(f'`{key}`' for key in not_members)}, which is no "
            f"member; it holds {', '.join(f'`{name}`' for name in member_names)}"
        )
    taken: "dict[str, Any]" = {}
    for member in members:
        if member["name"] in stored_value:
            held = stored_value[member["name"]]
        elif "default_value" in member:
            held = member["default_value"]
        else:
            raise ValueError(
                f"{what_it_is} leaves out the member `{member['name']}`, which has no default"
            )
        taken[member["name"]] = the_value_a_body_takes(
            member["member_type"], held, f"{what_it_is}'s member `{member['name']}`"
        )
    return taken


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
