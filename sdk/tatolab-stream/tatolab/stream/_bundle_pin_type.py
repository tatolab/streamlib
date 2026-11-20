# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A bundle pin: a frozen dataclass read into one pin type whose members are its fields.

Each field is read as a class node's field is, by `read_pin_declaration`, into a member: its
name, its type, its `PinMeaning` and its default. A member holds a value in every bundle, so
it is never `T | None`, states nothing more, and is no bundle itself; a type only a pin carries
and a named type are not carried in a bundle yet, and the members of a bundle that is a List's
element are small types.
"""

from __future__ import annotations

import dataclasses
import typing
from collections.abc import Callable
from typing import Optional

from ._contract_pin_types import BundleMember, PinType, PinTypeDescription
from ._description import Description
from ._formerly_named import FormerlyNamed
from ._named_pin_type_declaration import named_pin_type_declared_by
from ._pin_annotations_on_the_wire import ANNOTATION_NAME_OF_EACH_CLASS
from ._pin_spelling_words import (
    is_a_union,
    name_of,
    spellings_read_today,
    written_forms_a_list_element_takes,
)
from ._pin_type_choice import PinTypeFromSetting
from ._pin_types import PIN_VALUE_CLASSES, PinTypeSpelling

__all__ = [
    "bundle_of_the_fields_of",
    "dataclass_a_bundle_pin_is_written_as",
    "refuse_a_member_type_a_bundle_does_not_hold",
    "refuse_what_a_bundle_member_does_not_state",
]

type Refusal = Callable[[str, str], Exception]
"""What makes the refusal of the pin read, from its words and its finding code."""


def dataclass_a_bundle_pin_is_written_as(annotation: object) -> Optional[type]:
    """The dataclass a pin typed by a bundle, or a List of them, is written as, through any
    `Annotated` and `T | None`; `None` for a pin written otherwise."""
    while True:
        if typing.get_origin(annotation) is typing.Annotated:
            annotation = typing.get_args(annotation)[0]
            continue
        if is_a_union(annotation):
            the_rest = [member for member in typing.get_args(annotation) if member is not type(None)]
            if len(the_rest) != 1:
                return None
            annotation = the_rest[0]
            continue
        if typing.get_origin(annotation) is list and len(typing.get_args(annotation)) == 1:
            (annotation,) = typing.get_args(annotation)
            continue
        if (
            isinstance(annotation, type)
            and dataclasses.is_dataclass(annotation)
            and annotation not in PIN_VALUE_CLASSES
            and named_pin_type_declared_by(annotation) is None
        ):
            return annotation
        return None


def bundle_of_the_fields_of(
    written_type: type,
    *,
    pin_named: str,
    refusal: Refusal,
    member_of_a_field: "Callable[[dataclasses.Field[object], object], BundleMember]",
) -> PinType:
    """The bundle `written_type`, a dataclass, declares: one member for each of its fields, in
    order, each read by `member_of_a_field` from the field and its resolved annotation."""
    dataclass_params = getattr(written_type, "__dataclass_params__", None)
    if not getattr(dataclass_params, "frozen", False):
        raise refusal(
            f"{pin_named} is typed {name_of(written_type)}, a dataclass that is not frozen, and "
            f"a bundle is a value a node reads whole and never changes. To fix: "
            f"`@dataclass(frozen=True) class {written_type.__name__}`.",
            "pin_type_unknown",
        )
    fields = dataclasses.fields(written_type)
    if not fields:
        raise refusal(
            f"{pin_named} is typed {name_of(written_type)}, a dataclass with no field, and a "
            f"bundle holds at least one member. To fix: give {name_of(written_type)} a field.",
            "pin_type_unknown",
        )
    try:
        field_annotations = typing.get_type_hints(written_type, include_extras=True)
    except NameError as unresolved:
        raise refusal(
            f"{pin_named} is typed {name_of(written_type)}, whose fields name a type that does "
            f"not resolve: {unresolved}. To fix: import it in the module that declares "
            f"{name_of(written_type)}.",
            "pin_type_unknown",
        ) from None
    members = [member_of_a_field(field, field_annotations[field.name]) for field in fields]
    return {"type": "bundle", "members": members}  # pyright: ignore[reportReturnType]


def refuse_what_a_bundle_member_does_not_state(
    member_named: str,
    written_type: object,
    metadata: "list[object]",
    optional: bool,
    refusal: Refusal,
) -> None:
    """Refuse a member that may be `None`, whose link would find its type, or that carries
    what a member does not state: a member states its type, a default and a `PinMeaning`."""
    if optional:
        raise refusal(
            f"{member_named} is typed `T | None`, and every bundle holds a value of each of its "
            f"members. To fix: give the field a default of its type.",
            "pin_type_unknown",
        )
    if written_type is object or isinstance(written_type, typing.TypeVar) or is_a_union(written_type):
        raise refusal(
            f"{member_named} is typed {name_of(written_type)}, and a bundle member is of one "
            f"type. To fix: type it as one of {spellings_read_today()}.",
            "pin_type_unknown",
        )
    for entry in metadata:
        if isinstance(entry, (Description, FormerlyNamed, PinTypeFromSetting)) or (
            ANNOTATION_NAME_OF_EACH_CLASS.get(type(entry)) not in (None, "meaning")
        ):
            raise refusal(
                f"{member_named} carries {entry!r}, and a bundle member states only its type, its "
                f"default and a `PinMeaning`. To fix: drop it.",
                "pin_annotation_not_taken",
            )


def refuse_a_member_type_a_bundle_does_not_hold(
    member_named: str,
    member_of: str,
    written_type: object,
    spelling: PinTypeSpelling,
    row: PinTypeDescription,
    *,
    as_an_element: bool,
    of_a_list_element: bool,
    refusal: Refusal,
) -> None:
    """Refuse a member type a bundle does not hold: another bundle, a type only a pin carries
    or a named type, which no bundle carries yet, and a List in a bundle that is itself a
    List's element."""
    if spelling.written_form == "dataclass":
        raise refusal(
            f"{member_named} is typed {name_of(written_type)}, a dataclass, and a bundle holds no "
            f"bundle. To fix: write the fields of {name_of(written_type)} on the dataclass of "
            f"`{member_of}` itself.",
            "pin_type_not_read_yet",
        )
    if (spelling.only_a_pin_carries or spelling.written_form == "named_pin_type") and not as_an_element:
        raise refusal(
            f"{member_named} is typed {name_of(written_type)}, a {row.label}, which a bundle does "
            f"not carry yet. To fix: give it an output of its own beside `{member_of}`.",
            "pin_type_not_read_yet",
        )
    if spelling.written_form == "list" and of_a_list_element:
        raise refusal(
            f"{member_named} is typed {name_of(written_type)}, and `{member_of}` is a List of "
            f"bundles, whose members are small types, {written_forms_a_list_element_takes()}. "
            f"To fix: give the List an input or output of its own.",
            "list_element_not_taken",
        )
