# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The one reader of a typed pin: an annotation and a default read into the engine's `PinDeclaration`.

A pin is a name, an annotation and an optional default. `read_pin_declaration` reads the
annotation's type through the one table in `_pin_types`, its annotations through the classes
of `_pin_annotations`, the names it had through `FormerlyNamed`, and checks the default against
both; `refuse_former_names_that_find_no_one_pin` checks those names across a node's pins. Each
mistake is refused at decoration as a `NodeDeclarationRefusal`, a `TypeError` that carries a
finding code, the node and the pin, so the declaration check can answer it as data.
"""

from __future__ import annotations

import dataclasses
import math
import types
import typing
from collections.abc import Sequence
from dataclasses import dataclass
from typing import Any, Final, Literal, Optional, final

from ._contract_pin_types import (
    BUILT_IN_PIN_TYPES,
    BuiltInPinTypeName,
    PinAnnotationName,
    PinAnnotations,
    PinDeclarationOnTheWire,
    PinType,
    PinTypeDescription,
)
from ._description import Description
from ._formerly_named import FormerlyNamed
from ._number_range import NumberRange
from ._pin_annotations import NumberStep, NumberUnit, PinControl, PinMeaning
from ._pin_types import (
    INTEGER_BOUNDS_OF_EACH_SIZE,
    INTEGER_SIZES_ONLY_AN_ELEMENT_HOLDS,
    PIN_TYPE_SPELLINGS,
    PIN_VALUE_CLASSES,
    ExactNumberSize,
    Asset,
    PinValueOfParts,
    pin_type_spelling_written_as,
)

__all__ = [
    "NO_DEFAULT",
    "DeclaredPin",
    "NodeDeclarationFindingCode",
    "NodeDeclarationRefusal",
    "PinDirection",
    "PinValueNotTaken",
    "read_pin_declaration",
    "refuse_former_names_that_find_no_one_pin",
    "stored_form_of_a_pin_value",
]

type PinDirection = Literal["input", "output"]
"""Which side of its node a pin sits on."""

type NodeDeclarationFindingCode = Literal[
    "pin_has_no_type",
    "pin_typed_any",
    "pin_type_unknown",
    "pin_type_not_read_yet",
    "exact_size_only_as_an_element",
    "menu_options_not_valid",
    "list_element_not_taken",
    "pin_annotation_not_taken",
    "pin_annotation_repeated",
    "former_name_finds_no_one_pin",
    "value_input_has_no_default",
    "unset_input_with_a_default",
    "pin_default_not_of_its_type",
    "pin_default_outside_its_range",
    "parameter_not_a_pin",
    "return_not_read_yet",
]
"""What a refused declaration got wrong, as the declaration check's finding names it."""


class NodeDeclarationRefusal(TypeError):
    """A node declaration refused at decoration: the words, with their fix, and what was refused."""

    def __init__(
        self,
        words: str,
        *,
        finding_code: NodeDeclarationFindingCode,
        node: str,
        pin: Optional[str] = None,
    ) -> None:
        super().__init__(words)
        self.finding_code: NodeDeclarationFindingCode = finding_code
        """What the declaration got wrong."""
        self.node: str = node
        """The node type whose declaration was refused, by its short name."""
        self.pin: Optional[str] = pin
        """The pin the refusal is about, as written; `None` for one about the whole node."""


@final
class _NoDefault:
    """The default of a pin that states none."""

    __slots__ = ()

    def __repr__(self) -> str:
        return "NO_DEFAULT"


NO_DEFAULT: Final = _NoDefault()
"""What a pin with no default passes as its default."""


@dataclass(frozen=True, slots=True)
class DeclaredPin:
    """A pin read from its annotation and default."""

    declaration: PinDeclarationOnTheWire
    """What the engine reads of it."""

    optional: bool
    """Whether it is typed `T | None`: an input that may be unset, or an output a run may
    leave without a value."""

    default_value: object
    """An input's default in its stored JSON form, `None` for an input that may be unset, and
    `NO_DEFAULT` on an output."""


_PIN_TYPE_DESCRIPTION_BY_NAME: Final[dict[BuiltInPinTypeName, PinTypeDescription]] = {
    row.name: row for row in BUILT_IN_PIN_TYPES
}

_WRITTEN_FORMS_ONLY_AN_ELEMENT_TAKES: Final[frozenset[str]] = frozenset(
    ExactNumberSize(size).written_form for size in INTEGER_SIZES_ONLY_AN_ELEMENT_HOLDS
)

# Each annotation class, by the contract key it states.
_ANNOTATION_NAME_OF_EACH_CLASS: Final[dict[type, PinAnnotationName]] = {
    NumberRange: "range",
    NumberStep: "step",
    NumberUnit: "unit",
    PinMeaning: "meaning",
    PinControl: "control",
}

# The read types of the family grammar, by the typed spelling that replaces each.
_TYPED_SPELLING_OF_EACH_FAMILY_READ_TYPE_NAME: Final[dict[str, str]] = {
    "ImageFrame": "Texture",
    "ArrayFrame": "Tensor",
    "ChannelsBlock": "Audio",
    "PacketsUnit": "EncodedVideo` or `EncodedAudio",
    "UnreadMessage": "a type parameter",
}


def read_pin_declaration(
    node: str,
    pin_name: str,
    annotation: object,
    *,
    default: object = NO_DEFAULT,
    direction: PinDirection,
) -> DeclaredPin:
    """Read the pin `pin_name` of the node `node` from its annotation and default.

    `annotation` is the pin's whole annotation, `Annotated[...]` and `T | None` included, with
    any `Description` still on it. `default` is `NO_DEFAULT` for an input that states none and
    always for an output.
    """
    reading = _PinReading(node, pin_name, direction)
    return reading.read(annotation, default)


def refuse_former_names_that_find_no_one_pin(
    node: str, pins: "Sequence[tuple[str, str, PinDeclarationOnTheWire]]"
) -> None:
    """Refuse a name a pin of `node` had that would not find that pin alone.

    `pins` is each pin of the node, inputs and outputs, as its name as written, its port name
    and its declaration. A former name is refused when it is a pin's name now, its own pin's
    included, or when two pins list it, as the runtime refuses it at registration.
    """
    written_name_of_each_port = {port_name: written_name for written_name, port_name, _ in pins}
    written_name_listing_each_former_name: dict[str, str] = {}
    for written_name, _, declaration in pins:
        for former_name in declaration.get("former_names", []):
            holder = written_name_of_each_port.get(former_name)
            if holder == written_name:
                words = (
                    f"`{node}`'s pin `{written_name}` lists `{former_name}` as a name it had, "
                    f"and it is named that now. To fix: drop `{former_name}` from its "
                    f"`FormerlyNamed`."
                )
            elif holder is not None:
                words = (
                    f"`{node}`'s pin `{written_name}` lists `{former_name}` as a name it had, "
                    f"and pin `{holder}` is named that now, so a project saved under "
                    f"`{former_name}` would not find one pin. To fix: drop `{former_name}` "
                    f"from its `FormerlyNamed`, or rename `{holder}`."
                )
            elif former_name in written_name_listing_each_former_name:
                other = written_name_listing_each_former_name[former_name]
                words = (
                    f"`{node}`'s pins `{other}` and `{written_name}` both list `{former_name}` "
                    f"as a name they had, so a project saved under it would not find one pin. "
                    f"To fix: keep `{former_name}` on the pin that had it."
                )
            else:
                written_name_listing_each_former_name[former_name] = written_name
                continue
            raise NodeDeclarationRefusal(
                words, finding_code="former_name_finds_no_one_pin", node=node, pin=written_name
            )


class _PinReading:
    """One pin's annotation and default, read once."""

    def __init__(self, node: str, pin_name: str, direction: PinDirection) -> None:
        self._node = node
        self._pin_name = pin_name
        self._direction = direction

    @property
    def _pin_named(self) -> str:
        return f"`{self._node}`'s {self._direction} `{self._pin_name}`"

    def _refusal(self, words: str, finding_code: NodeDeclarationFindingCode) -> NodeDeclarationRefusal:
        return NodeDeclarationRefusal(
            words, finding_code=finding_code, node=self._node, pin=self._pin_name
        )

    def read(self, annotation: object, default: object) -> DeclaredPin:
        written_type, metadata, optional = self._written_type_metadata_and_optional(annotation)
        exact_size = self._exact_size_among(metadata)
        pin_type = self._pin_type_of(written_type, exact_size)
        row = _PIN_TYPE_DESCRIPTION_BY_NAME[pin_type["type"]]
        annotations = self._annotations_among(metadata, row)
        declaration: PinDeclarationOnTheWire = {
            "pin_type": {"kind": "is", "pin_type": pin_type},
            "flow": row.flows[0],
        }
        if annotations:
            declaration["annotations"] = annotations
        formerly_named = self._formerly_named_among(metadata)
        if formerly_named is not None:
            declaration["former_names"] = list(formerly_named.port_names)
        if self._direction == "output":
            return DeclaredPin(declaration, optional=optional, default_value=NO_DEFAULT)
        if optional:
            declaration["may_be_unset"] = True
        default_value = self._default_value(default, pin_type, row, annotations, optional)
        return DeclaredPin(declaration, optional=optional, default_value=default_value)

    def _written_type_metadata_and_optional(
        self, annotation: object
    ) -> "tuple[object, list[object], bool]":
        """The type an annotation names, the `Annotated` metadata around it, and whether it is `T | None`."""
        metadata: list[object] = []
        optional = False
        written_type = annotation
        while True:
            if typing.get_origin(written_type) is typing.Annotated:
                written_type, *carried = typing.get_args(written_type)
                metadata.extend(carried)
                continue
            members = typing.get_args(written_type) if _is_a_union(written_type) else ()
            if type(None) in members:
                if optional or len(members) != 2:
                    break
                (written_type,) = [member for member in members if member is not type(None)]
                optional = True
                continue
            break
        return written_type, metadata, optional

    def _exact_size_among(self, metadata: "list[object]") -> Optional[ExactNumberSize]:
        sizes = [entry for entry in metadata if isinstance(entry, ExactNumberSize)]
        if len(sizes) > 1:
            raise self._refusal(
                f"{self._pin_named} states {len(sizes)} exact sizes, "
                f"{', '.join(size.written_form for size in sizes)}; a number has one. "
                f"To fix: keep one.",
                "pin_annotation_repeated",
            )
        return sizes[0] if sizes else None

    def _formerly_named_among(self, metadata: "list[object]") -> Optional[FormerlyNamed]:
        written = [entry for entry in metadata if isinstance(entry, FormerlyNamed)]
        if len(written) > 1:
            raise self._refusal(
                f"{self._pin_named} carries {len(written)} `FormerlyNamed`s; it takes one. To "
                f"fix: list every name it had in one, `FormerlyNamed(\"last_name\", "
                f"\"the_one_before\")`.",
                "pin_annotation_repeated",
            )
        return written[0] if written else None

    def _pin_type_of(
        self,
        written_type: object,
        exact_size: Optional[ExactNumberSize],
        *,
        as_an_element: bool = False,
    ) -> PinType:
        written_form = self._written_form_of(written_type, exact_size)
        spelling = pin_type_spelling_written_as(written_form)
        row = _PIN_TYPE_DESCRIPTION_BY_NAME[spelling.pin_type_name]
        if spelling.not_read_yet_because is not None:
            typed_as = (
                f"{_name_of(written_type)}, a dataclass, so"
                if written_form == "dataclass"
                else f"`{written_form}`,"
            )
            raise self._refusal(
                f"{self._pin_named} is typed {typed_as} the {row.label} pin type, which "
                f"the typed grammar does not read yet: {spelling.not_read_yet_because}. To "
                f"fix: type it as one of {_spellings_read_today()}.",
                "pin_type_not_read_yet",
            )
        if _is_a_value_class_of_parts(written_type):
            return dict(written_type.pin_type)  # pyright: ignore[reportReturnType]
        if written_form == "Literal":
            return self._menu_pin_type_of(written_type)
        if written_form == "list":
            return self._list_pin_type_of(written_type)
        if exact_size is None:
            pin_type: PinType = {"type": spelling.pin_type_name}  # pyright: ignore[reportAssignmentType]
            return pin_type
        if exact_size.size in INTEGER_SIZES_ONLY_AN_ELEMENT_HOLDS and not as_an_element:
            raise self._refusal(
                f"{self._pin_named} is typed `{written_form}`, and a single pin holds a whole "
                f"number exact to 2^53 at most, so `{written_form}` types only an element of a "
                f"List or a Tensor. To fix: type it `int`, or `Int32`.",
                "exact_size_only_as_an_element",
            )
        if exact_size.size in INTEGER_BOUNDS_OF_EACH_SIZE:
            return {"type": "integer", "exact_size": exact_size.size}  # pyright: ignore[reportReturnType]
        return {"type": "float", "exact_size": exact_size.size}  # pyright: ignore[reportReturnType]

    def _menu_pin_type_of(self, written_type: object) -> PinType:
        option_names = typing.get_args(written_type)
        if not all(isinstance(option, str) for option in option_names):
            raise self._refusal(
                f"{self._pin_named} is typed {_name_of(written_type)}, and a Menu's options are "
                f"names. To fix: list strings, `Literal[\"sine\", \"square\"]`.",
                "menu_options_not_valid",
            )
        if len(set(option_names)) != len(option_names):
            raise self._refusal(
                f"{self._pin_named} is typed {_name_of(written_type)}, which lists an option "
                f"twice. To fix: list each option once.",
                "menu_options_not_valid",
            )
        return {"type": "menu", "option_names": list(option_names)}  # pyright: ignore[reportReturnType]

    def _list_pin_type_of(self, written_type: object) -> PinType:
        element_annotations = typing.get_args(written_type)
        if len(element_annotations) != 1:
            raise self._refusal(
                f"{self._pin_named} is typed {_name_of(written_type)}, and a List names the type "
                f"of its elements. To fix: `list[float]`, or a List of another type.",
                "list_element_not_taken",
            )
        element_type, metadata, optional = self._written_type_metadata_and_optional(
            element_annotations[0]
        )
        not_an_element = [
            entry for entry in metadata if not isinstance(entry, (ExactNumberSize, Description))
        ]
        if optional or not_an_element:
            raise self._refusal(
                f"{self._pin_named} is typed {_name_of(written_type)}, and a List element is "
                f"a value of one type with no annotation of its own, never `None`. To fix: "
                f"annotate the List, `Annotated[list[float], ...]`.",
                "list_element_not_taken",
            )
        element_form = self._written_form_of(element_type, self._exact_size_among(metadata))
        if element_form not in _WRITTEN_FORMS_A_LIST_ELEMENT_TAKES:
            raise self._refusal(
                f"{self._pin_named} is typed {_name_of(written_type)}, and a List element is a "
                f"small type, {_written_forms_a_list_element_takes()}; `{element_form}` is not. "
                f"To fix: type its elements as one of them.",
                "list_element_not_taken",
            )
        element_pin_type = self._pin_type_of(
            element_type, self._exact_size_among(metadata), as_an_element=True
        )
        return {"type": "list_of", "element_type": element_pin_type}  # pyright: ignore[reportReturnType]

    def _written_form_of(self, written_type: object, exact_size: Optional[ExactNumberSize]) -> str:
        if written_type is typing.Any:
            raise self._refusal(
                f"{self._pin_named} is typed `Any`, which checks nothing. To fix: type it as "
                f"one of {_spellings_read_today()}.",
                "pin_typed_any",
            )
        if exact_size is not None:
            holds = int if exact_size.size in INTEGER_BOUNDS_OF_EACH_SIZE else float
            if written_type is not holds:
                raise self._refusal(
                    f"{self._pin_named} states the exact size `{exact_size.written_form}` on "
                    f"{_name_of(written_type)}, and it sizes `{holds.__name__}`. To fix: type "
                    f"it `{exact_size.written_form}`.",
                    "pin_type_unknown",
                )
            return exact_size.written_form
        if written_type is bool:
            return "bool"
        if written_type is int:
            return "int"
        if written_type is float:
            return "float"
        if written_type is str:
            return "str"
        if typing.get_origin(written_type) is Literal:
            return "Literal"
        if written_type is list or typing.get_origin(written_type) is list:
            return "list"
        if _is_a_value_class_of_parts(written_type) or written_type is Asset:
            return written_type.__name__
        typed_spelling = _TYPED_SPELLING_OF_EACH_FAMILY_READ_TYPE_NAME.get(
            getattr(written_type, "__name__", "")
        )
        if typed_spelling is not None and getattr(written_type, "__module__", "").startswith(
            "tatolab.stream"
        ):
            raise self._refusal(
                f"{self._pin_named} is typed {_name_of(written_type)}, a read type of a port "
                f"family, and a pin under bare `@node` is typed by its pin type, "
                f"`{typed_spelling}`, which the typed grammar does not read yet. To fix: keep "
                f"this node on `@node.function` until it does.",
                "pin_type_not_read_yet",
            )
        if isinstance(written_type, type) and dataclasses.is_dataclass(written_type):
            return "dataclass"
        if _is_a_union(written_type):
            raise self._refusal(
                f"{self._pin_named} is typed {_name_of(written_type)}, one of several types, "
                f"which the typed grammar does not read yet. To fix: type it as one of "
                f"{_spellings_read_today()}.",
                "pin_type_not_read_yet",
            )
        raise self._refusal(
            f"{self._pin_named} is typed {_name_of(written_type)}, which is no pin type. To "
            f"fix: type it as one of {_spellings_read_today()}.",
            "pin_type_unknown",
        )

    def _annotations_among(
        self, metadata: "list[object]", row: PinTypeDescription
    ) -> PinAnnotations:
        annotations: PinAnnotations = {}
        for entry in metadata:
            if isinstance(entry, (ExactNumberSize, Description, FormerlyNamed)):
                continue
            annotation_name = _ANNOTATION_NAME_OF_EACH_CLASS.get(type(entry))
            if annotation_name is None:
                raise self._refusal(
                    f"{self._pin_named} carries {entry!r}, which no pin reads. To fix: drop "
                    f"it; a pin reads `NumberRange`, `NumberStep`, `NumberUnit`, `PinMeaning`, "
                    f"`PinControl`, `FormerlyNamed` and `Description`.",
                    "pin_annotation_not_taken",
                )
            if annotation_name in annotations:
                raise self._refusal(
                    f"{self._pin_named} carries two `{type(entry).__name__}`s; it takes one. "
                    f"To fix: keep one.",
                    "pin_annotation_repeated",
                )
            if annotation_name not in row.annotations:
                raise self._refusal(
                    f"{self._pin_named} carries `{type(entry).__name__}`, which a {row.label} "
                    f"pin does not take; it takes {_annotation_classes_named(row)}. To fix: "
                    f"drop it.",
                    "pin_annotation_not_taken",
                )
            annotations[annotation_name] = _annotation_on_the_wire(entry)
        return annotations

    def _default_value(
        self,
        default: object,
        pin_type: PinType,
        row: PinTypeDescription,
        annotations: PinAnnotations,
        optional: bool,
    ) -> object:
        if optional:
            if default is not None:
                raise self._refusal(
                    f"{self._pin_named} is typed `T | None`, so it reads `None` while unset, "
                    f"and it defaults to {default!r}. To fix: `= None`, or drop `| None` to "
                    f"keep the default.",
                    "unset_input_with_a_default",
                )
            return None
        if default is NO_DEFAULT:
            raise self._refusal(
                f"{self._pin_named} has no default and takes typed values, so it would read "
                f"nothing until a value is set. To fix: give it a default of its type, or "
                f"type it `{row.python_forms[0]} | None = None` so it may be unset.",
                "value_input_has_no_default",
            )
        try:
            return stored_form_of_a_pin_value(default, pin_type, annotations)
        except PinValueNotTaken as not_taken:
            raise self._refusal(
                f"{self._pin_named} defaults to {not_taken.value!r}, {not_taken.why}. To fix: "
                f"{self._fix_of_a_default_not_taken(not_taken)}",
                (
                    "pin_default_outside_its_range"
                    if not_taken.kind in ("outside_its_exact_size", "outside_its_range")
                    else "pin_default_not_of_its_type"
                ),
            ) from None

    @staticmethod
    def _fix_of_a_default_not_taken(not_taken: PinValueNotTaken) -> str:
        match not_taken.kind:
            case "not_of_its_type":
                python_form = _PIN_TYPE_DESCRIPTION_BY_NAME[not_taken.pin_type["type"]].python_forms[0]
                return (
                    f"give it a default of its type, or type it `{python_form} | None = None` "
                    f"so it may be unset."
                )
            case "outside_its_exact_size":
                return "give it a default that fits."
            case "not_finite":
                return "give it a finite default."
            case "not_an_option":
                return "default to one of them."
            case "outside_its_range":
                return "give it a default inside its range."


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
    row = _PIN_TYPE_DESCRIPTION_BY_NAME[pin_type["type"]]
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
        case _:
            raise not_of_its_type


# The written forms of the small types, which a List element takes.
_WRITTEN_FORMS_A_LIST_ELEMENT_TAKES: Final[frozenset[str]] = frozenset(
    spelling.written_form
    for spelling in PIN_TYPE_SPELLINGS
    if spelling.pin_type_name
    in (
        "boolean",
        "integer",
        "float",
        "string",
        "menu",
        "color",
        "vector",
        "rectangle",
        "transform",
        "asset",
    )
)


def _written_forms_a_list_element_takes() -> str:
    return ", ".join(
        f"`{spelling.written_form}`"
        for spelling in PIN_TYPE_SPELLINGS
        if spelling.written_form in _WRITTEN_FORMS_A_LIST_ELEMENT_TAKES
    )


def _is_a_value_class_of_parts(written_type: object) -> typing.TypeGuard[type[PinValueOfParts]]:
    return written_type in PIN_VALUE_CLASSES


def _the_value_class_of(pin_type: PinType) -> type[PinValueOfParts]:
    (value_class,) = [
        value_class for value_class in PIN_VALUE_CLASSES if value_class.pin_type == pin_type
    ]
    return value_class


def _annotation_on_the_wire(annotation: object) -> Any:
    match annotation:
        case NumberRange():
            return {
                bound: value
                for bound, value in (("minimum", annotation.minimum), ("maximum", annotation.maximum))
                if value is not None
            }
        case NumberStep():
            return annotation.step
        case NumberUnit() | PinMeaning() | PinControl():
            return annotation.text
        case _:
            raise AssertionError(f"{annotation!r} is not a pin annotation")


def _annotation_classes_named(row: PinTypeDescription) -> str:
    names = [
        f"`{annotation_class.__name__}`"
        for annotation_class, annotation_name in _ANNOTATION_NAME_OF_EACH_CLASS.items()
        if annotation_name in row.annotations
    ]
    return ", ".join(names) if names else "none of them"


def _spellings_read_today() -> str:
    read_today = [
        f"`{spelling.written_form}`"
        for spelling in PIN_TYPE_SPELLINGS
        if spelling.not_read_yet_because is None
        and spelling.written_form not in _WRITTEN_FORMS_ONLY_AN_ELEMENT_TAKES
    ]
    return ", ".join(read_today)


def _is_a_union(annotation: object) -> bool:
    return typing.get_origin(annotation) in (typing.Union, types.UnionType)


def _name_of(annotation: object) -> str:
    if isinstance(annotation, type):
        return f"`{annotation.__name__}`"
    return f"`{annotation!r}`"
