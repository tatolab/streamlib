# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The one reader of a typed pin: an annotation and a default read into the engine's `PinDeclaration`.

A pin is a name, an annotation and an optional default. `read_pin_declaration` reads the
annotation's type through the one table in `_pin_types`, its annotations through the classes
of `_pin_annotations`, the names it had through `FormerlyNamed`, and checks the default against
both; `_former_name_check` checks those names across a node's pins. Each
mistake is refused at decoration as a `NodeDeclarationRefusal`, a `TypeError` that carries a
finding code, the node and the pin, so the declaration check can answer it as data.

A pin whose type its link finds is wire only and takes no default: `object` and an unbounded
type parameter take any type, a union and a type parameter bounded by one take each of its
types, and an output typed by a type parameter takes the type linked into the one input that
parameter types. A type only a pin carries, such as `Texture`, holds no stored value, so a
function takes it only as a type parameter's bound, whose value its body hands on unread, and
only when it flows as a Value, since a function reads the newest message on each input.
"""

from __future__ import annotations

import dataclasses
import typing
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Final, Literal, Optional, final

from ._contract_pin_types import (
    BundleMember,
    PinAnnotations,
    DeclaredPinType,
    PinDeclarationOnTheWire,
    PinFlow,
    PinType,
    PinTypeDescription,
)
from ._bundle_pin_type import (
    HEAVY_MEMBER_TYPES_A_BUNDLE_CARRIES,
    bundle_of_the_fields_of,
    holds_a_member_carried_by_reference,
    refuse_a_member_type_a_bundle_does_not_hold,
    refuse_what_a_bundle_member_does_not_state,
    words_refusing_a_bundle_only_a_pin_carries,
)
from ._description import Description
from ._node_declaration_refusal import NodeDeclarationFindingCode, NodeDeclarationRefusal
from ._formerly_named import FormerlyNamed
from ._input_declaration_annotations import INPUT_DECLARATION_ANNOTATION_CLASSES
from ._module_declared_pin_type import the_pin_type_of_a_module_declared_type, the_spelling_of_a_module_declared_type
from ._node_reference_class import is_the_node_reference_class
from ._pin_annotations_on_the_wire import (
    ANNOTATION_NAME_OF_EACH_CLASS,
    annotation_classes_named,
    annotation_on_the_wire,
)
from ._pin_declaration_refusal_words import (
    TypeParameterLimit,
    words_refusing_a_function_pin_of,
    PIN_TYPES_A_PIN_OF_SEVERAL_TYPES_TAKES,
    words_refusing_an_input_declaration_annotation_on,
    words_refusing_a_pin_of_several_types_taking,
    words_refusing_a_type_parameter_limited_to,
)
from ._pin_default_refusal_words import fix_of_a_default_not_taken
from ._pin_spelling_words import WRITTEN_FORMS_A_LIST_ELEMENT_TAKES
from ._pin_spelling_words import is_a_union as _is_a_union
from ._pin_spelling_words import name_of as _name_of
from ._pin_spelling_words import spellings_read_today as _spellings_read_today
from ._pin_spelling_words import written_forms_a_list_element_takes as _written_forms_a_list_element_takes
from ._pin_spelling_words import written_name_of as _written_name_of
from ._pin_type_choice import PinTypeChoice, PinTypeFromSetting
from ._pin_type_narrowing import FlowsAsValue, PinAnnotationNotTaken, PinTypeNarrowing
from ._pin_type_narrowing import an_output_flowing_as_a_value_by, pin_type_narrowed_by
from ._pin_typed_by_a_setting import declaration_of_a_pin_a_setting_types
from ._pin_typed_by_a_type_parameter import (
    output_pin_type_found_from_an_input,
    the_flow_a_function_input_reads,
    the_one_type_a_class_type_parameter_binds,
)
from ._pin_value_stored_form import (
    PIN_TYPE_DESCRIPTION_BY_NAME as _PIN_TYPE_DESCRIPTION_BY_NAME,
    PinValueNotTaken,
    stored_form_of_a_pin_value,
)
from ._pin_types import (
    INTEGER_BOUNDS_OF_EACH_SIZE,
    INTEGER_SIZES_ONLY_AN_ELEMENT_HOLDS,
    PIN_VALUE_CLASSES,
    STREAM_PIN_TYPE_CLASSES,
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
    "PinForm",
    "PinValueNotTaken",
    "read_pin_declaration",
    "stored_form_of_a_pin_value",
]

type PinDirection = Literal["input", "output"]
"""Which side of its node a pin sits on."""

type PinForm = Literal["function", "field", "pin"]
"""How a pin is written: `function`, a function or shader node's parameter or return, which
holds a value; `field`, a class node's annotated field, a value input; `pin`, a class node's
`InputPin[...]` or `OutputPin[...]`, which streams or fires."""

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
    `NO_DEFAULT` on an output, on a wire only input that may not be unset, on a class node's
    `InputPin`, which reads what its wire carries, and on a built-in's setting a graph must set."""

    type_parameter: Optional[typing.TypeVar] = None
    """The type parameter that types it, whose value its node can only hand on; `None` for a
    pin typed otherwise."""

    chosen_among: "tuple[PinType, ...]" = ()
    """Each type the setting typing a module's output may choose; empty for any other pin."""


def read_pin_declaration(
    node: str,
    pin_name: str,
    annotation: object,
    *,
    default: object = NO_DEFAULT,
    direction: PinDirection,
    form: PinForm = "function",
    input_ports_of_each_type_parameter: "Mapping[typing.TypeVar, Sequence[str]]" = {},  # noqa: B006
    of_the_runtimes_own_node: bool = False,
    a_setting: bool = False,
) -> DeclaredPin:
    """Read the pin `pin_name` of the node `node` from its annotation and default.

    `annotation` is the pin's whole annotation, `Annotated[...]` and `T | None` included, with
    any `Description` still on it; for a class node's pin, the type inside `InputPin[...]` or
    `OutputPin[...]`. `default` is `NO_DEFAULT` for an input that states none and always for an
    output and a pin. `form` is how the pin is written, which decides its flow and the types
    it takes. `input_ports_of_each_type_parameter` names, by port, the node's inputs each type
    parameter types, which an output typed by one follows.

    A pin `of_the_runtimes_own_node`, a built-in's, may say three things an author's may not: a
    setting, `a_setting`, with no default, which a graph must set; a `PinTypeChoice` field; and
    a pin written `Annotated[object, PinTypeFromSetting("...")]`, typed by that setting.
    """
    reading = _PinReading(
        node,
        pin_name,
        direction,
        form,
        of_the_runtimes_own_node=of_the_runtimes_own_node,
        a_setting=a_setting,
    )
    return reading.read(annotation, default, input_ports_of_each_type_parameter)


class _PinReading:
    """One pin's annotation and default, read once."""

    def __init__(
        self,
        node: str,
        pin_name: str,
        direction: PinDirection,
        form: PinForm,
        *,
        of_the_runtimes_own_node: bool = False,
        a_setting: bool = False,
        member_of: Optional[str] = None,
        of_a_list_element: bool = False,
    ) -> None:
        self._node = node
        self._pin_name = pin_name
        self._direction: PinDirection = direction
        self._form: PinForm = form
        self._of_the_runtimes_own_node = of_the_runtimes_own_node
        self._a_setting = a_setting
        self._member_of = member_of
        """The pin whose bundle this is a member of, a field of its dataclass; `None` for a pin."""
        self._of_a_list_element = of_a_list_element
        """Whether the bundle this is a member of is a List's element, whose members are small types."""
        self._written_type: object = None

    @property
    def _pin_named(self) -> str:
        if self._member_of is not None:
            return (
                f"`{self._node}`'s {self._direction} `{self._member_of}`'s member "
                f"`{self._pin_name}`"
            )
        return f"`{self._node}`'s {self._direction} `{self._pin_name}`"

    def _refusal(self, words: str, finding_code: NodeDeclarationFindingCode) -> NodeDeclarationRefusal:
        return NodeDeclarationRefusal(
            words,
            finding_code=finding_code,
            node=self._node,
            pin=self._pin_name if self._member_of is None else self._member_of,
        )

    def read(
        self,
        annotation: object,
        default: object,
        input_ports_of_each_type_parameter: "Mapping[typing.TypeVar, Sequence[str]]",
    ) -> DeclaredPin:
        written_type, metadata, optional = self._written_type_metadata_and_optional(annotation)
        self._written_type = written_type
        if self._member_of is not None:
            refuse_what_a_bundle_member_does_not_state(
                self._pin_named, written_type, metadata, optional, self._refusal  # pyright: ignore[reportArgumentType]
            )
        type_settings = [entry for entry in metadata if isinstance(entry, PinTypeFromSetting)]
        if type_settings:
            return self._read_a_pin_a_setting_types(
                written_type, metadata, type_settings, optional, default
            )
        if written_type is typing.Any:
            raise self._refusal(
                f"{self._pin_named} is typed `Any`, which checks nothing. To fix: "
                + (
                    "`object`, or a type parameter when an output hands the value on."
                    if self._direction == "input"
                    else "a type parameter that also types the input whose value it hands on."
                ),
                "pin_typed_any",
            )
        if written_type is object and self._of_the_runtimes_own_node and self._form == "pin":
            return self._read_a_pin_of_the_runtimes_own_node_its_link_types(metadata)
        if isinstance(written_type, typing.TypeVar) and self._form == "pin":
            return self._read_a_pin_a_class_type_parameter_types(
                written_type, metadata, optional, input_ports_of_each_type_parameter
            )
        if (
            written_type is object
            or isinstance(written_type, typing.TypeVar)
            or _is_a_union(written_type)
        ):
            if self._form != "function" and not self._a_union_input_pin(written_type):
                raise self._refusal(
                    f"{self._pin_named} is typed {_name_of(written_type)}, which its link finds "
                    f"the type of, and a class node's field or pin states one pin type. To fix: "
                    f"type it as one of {_spellings_read_today()}.",
                    "pin_type_not_read_yet",
                )
            return self._read_a_pin_whose_link_finds_its_type(
                written_type, metadata, optional, default, input_ports_of_each_type_parameter
            )
        exact_size = self._exact_size_among(metadata)
        pin_type = self._pin_type_of(written_type, exact_size)
        row = _PIN_TYPE_DESCRIPTION_BY_NAME[pin_type["type"]]
        try:
            pin_type = pin_type_narrowed_by(metadata, pin_type, self._pin_named)
            flows_as_value = an_output_flowing_as_a_value_by(
                metadata, pin_type, self._pin_named, direction=self._direction
            )
        except PinAnnotationNotTaken as not_taken:
            raise self._refusal(not_taken.words, not_taken.finding_code) from None
        taken_off = (PinTypeNarrowing, FlowsAsValue)
        metadata = [entry for entry in metadata if not isinstance(entry, taken_off)]
        annotations = self._annotations_among(metadata, row)
        declaration: PinDeclarationOnTheWire = {
            "pin_type": {"kind": "is", "pin_type": pin_type},
            "flow": "value" if flows_as_value else self._flow_of(row, pin_type),
        }
        if annotations:
            declaration["annotations"] = annotations
        self._state_former_names(declaration, metadata)
        if self._direction == "output":
            return DeclaredPin(declaration, optional=optional, default_value=NO_DEFAULT)
        if optional:
            declaration["may_be_unset"] = True
        if self._form == "pin":
            return DeclaredPin(declaration, optional=optional, default_value=NO_DEFAULT)
        if row.value_component is None:
            return self._read_an_input_holding_no_value(declaration, row, optional, default)
        default_value = self._default_value(default, pin_type, row, annotations, optional)
        return DeclaredPin(declaration, optional=optional, default_value=default_value)

    def _read_a_pin_a_setting_types(
        self,
        written_type: object,
        metadata: "list[object]",
        type_settings: "list[PinTypeFromSetting]",
        optional: bool,
        default: object,
    ) -> DeclaredPin:
        """A pin carrying `PinTypeFromSetting("...")`, of the type that setting chooses."""
        declaration, chosen_among = declaration_of_a_pin_a_setting_types(
            pin_named=self._pin_named,
            written_type=written_type,
            metadata=metadata,
            type_settings=type_settings,
            of_the_runtimes_own_node=self._of_the_runtimes_own_node,
            direction=self._direction,
            form=self._form,
            optional=optional,
            default=default,
            member_pin_type=self._a_member_pin_type,
            refusal=self._refusal,
        )
        self._state_former_names(declaration, metadata)
        takes_none = self._direction != "output" and optional
        return DeclaredPin(
            declaration,
            optional=optional,
            default_value=None if takes_none else NO_DEFAULT,
            chosen_among=chosen_among,
        )

    def _read_an_input_holding_no_value(
        self,
        declaration: PinDeclarationOnTheWire,
        row: PinTypeDescription,
        optional: bool,
        default: object,
    ) -> DeclaredPin:
        """An input of a type that stores no value: wire only, reading `None` while unlinked when it may be unset."""
        if optional and default is not None:
            raise self._refusal(
                f"{self._pin_named} is typed `T | None`, so it reads `None` while unlinked, and "
                f"it defaults to {default!r}. To fix: `= None`.",
                "unset_input_with_a_default",
            )
        if not optional and default is not NO_DEFAULT:
            raise self._refusal(
                f"{self._pin_named} is a {row.label}, which holds no value, so it reads only "
                f"what a link brings, and it defaults to {default!r}. To fix: drop the default, "
                f"or type it `T | None = None` so it reads `None` while unlinked.",
                "wire_only_input_with_a_default",
            )
        return DeclaredPin(
            declaration, optional=optional, default_value=None if optional else NO_DEFAULT
        )

    def _flow_of(self, row: PinTypeDescription, pin_type: PinType) -> PinFlow:
        """How the pin reads its messages: a field's value, an `InputPin`'s stream or event, an
        output of a bundle holding a Texture a stream, else its type's first flow."""
        if self._form == "function" or self._direction == "output":
            if self._direction == "output" and holds_a_member_carried_by_reference(pin_type):
                return "stream"
            return row.flows[0]
        if self._form == "field":
            return "value"
        flow: PinFlow = "trigger" if row.flows == ("trigger",) else "stream"
        if flow not in row.flows:
            raise self._refusal(
                f"{self._pin_named} is an `InputPin` of a {row.label}, which does not stream. "
                f"To fix: declare it as a field with its default, "
                f"`{self._pin_name}: {row.python_forms[0]} = ...`.",
                "pin_flow_not_taken",
            )
        return flow

    def _state_former_names(
        self, declaration: PinDeclarationOnTheWire, metadata: "list[object]"
    ) -> None:
        formerly_named = self._formerly_named_among(metadata)
        if formerly_named is not None:
            declaration["former_names"] = list(formerly_named.port_names)

    def _read_a_pin_whose_link_finds_its_type(
        self,
        written_type: object,
        metadata: "list[object]",
        optional: bool,
        default: object,
        input_ports_of_each_type_parameter: "Mapping[typing.TypeVar, Sequence[str]]",
    ) -> DeclaredPin:
        """A pin typed `object`, a type parameter or a union: wire only, stating no annotation."""
        for entry in metadata:
            if not isinstance(entry, (Description, FormerlyNamed)):
                raise self._refusal(
                    f"{self._pin_named} is typed {_name_of(written_type)}, which its link finds "
                    f"the type of, and carries {entry!r}, which only a pin of one type takes. "
                    f"To fix: drop it; such a pin takes `FormerlyNamed` and `Description`.",
                    "pin_annotation_not_taken",
                )
        type_parameter = written_type if isinstance(written_type, typing.TypeVar) else None
        if self._direction == "output":
            pin_type = output_pin_type_found_from_an_input(
                pin_named=self._pin_named,
                written_type=written_type,
                type_parameter=type_parameter,
                input_ports_of_each_type_parameter=input_ports_of_each_type_parameter,
                refusal=self._refusal,
            )
            declaration: PinDeclarationOnTheWire = {"pin_type": pin_type, "flow": "value"}
            self._state_former_names(declaration, metadata)
            return DeclaredPin(
                declaration, optional=optional, default_value=NO_DEFAULT, type_parameter=type_parameter
            )
        pin_type, flow = self._input_pin_type_its_link_finds(written_type, type_parameter)
        declaration = {"pin_type": pin_type, "flow": flow}
        self._state_former_names(declaration, metadata)
        if optional:
            declaration["may_be_unset"] = True
        if self._form == "pin":
            return DeclaredPin(declaration, optional=optional, default_value=NO_DEFAULT)
        if optional and default is not None:
            raise self._refusal(
                f"{self._pin_named} is typed `T | None`, so it reads `None` while unlinked, and "
                f"it defaults to {default!r}. To fix: `= None`.",
                "unset_input_with_a_default",
            )
        if not optional and default is not NO_DEFAULT:
            raise self._refusal(
                f"{self._pin_named} is typed {_name_of(written_type)}, whose type its link "
                f"finds, so it reads only what a link brings and holds no value, and it "
                f"defaults to {default!r}. To fix: drop the default, or type it "
                f"`{_written_name_of(written_type)} | None = None` so it reads `None` while "
                f"unlinked.",
                "wire_only_input_with_a_default",
            )
        return DeclaredPin(
            declaration,
            optional=optional,
            default_value=None if optional else NO_DEFAULT,
            type_parameter=type_parameter,
        )

    def _read_a_pin_a_class_type_parameter_types(
        self,
        type_parameter: typing.TypeVar,
        metadata: "list[object]",
        optional: bool,
        input_ports_of_each_type_parameter: "Mapping[typing.TypeVar, Sequence[str]]",
    ) -> DeclaredPin:
        """A class node's pin typed by its type parameter: an input of the one type it is bound
        to, or an output carrying the type linked into the one input it also types."""
        bound = the_one_type_a_class_type_parameter_binds(
            pin_named=self._pin_named,
            node=self._node,
            type_parameter=type_parameter,
            metadata=metadata,
            optional=optional,
            refusal=self._refusal,
        )
        pin_type = self._a_member_pin_type(bound, limit="bound to")
        flow = self._flow_of(_PIN_TYPE_DESCRIPTION_BY_NAME[pin_type["type"]], pin_type)
        declaration: PinDeclarationOnTheWire = {
            "pin_type": (
                output_pin_type_found_from_an_input(
                    pin_named=self._pin_named,
                    written_type=type_parameter,
                    type_parameter=type_parameter,
                    input_ports_of_each_type_parameter=input_ports_of_each_type_parameter,
                    refusal=self._refusal,
                )
                if self._direction == "output"
                else {"kind": "is", "pin_type": pin_type}
            ),
            "flow": flow,
        }
        self._state_former_names(declaration, metadata)
        return DeclaredPin(
            declaration, optional=False, default_value=NO_DEFAULT, type_parameter=type_parameter
        )

    def _read_a_pin_of_the_runtimes_own_node_its_link_types(
        self, metadata: "list[object]"
    ) -> DeclaredPin:
        """A pin of the runtime's own node typed `object`, which carries whatever type the port
        linked to it carries: a variable's `value`, of the type of the port its setter is linked
        from. It states no annotation."""
        for entry in metadata:
            if not isinstance(entry, Description):
                raise self._refusal(
                    f"{self._pin_named} is typed `object`, which its link finds the type of, and "
                    f"carries {entry!r}. To fix: drop it; such a pin takes `Description`.",
                    "pin_annotation_not_taken",
                )
        declaration: PinDeclarationOnTheWire = {"pin_type": {"kind": "any"}, "flow": "value"}
        return DeclaredPin(declaration, optional=False, default_value=NO_DEFAULT)

    def _input_pin_type_its_link_finds(
        self, written_type: object, type_parameter: Optional[typing.TypeVar]
    ) -> "tuple[DeclaredPinType, PinFlow]":
        if type_parameter is not None:
            if type_parameter.__constraints__:
                return self._one_of(type_parameter.__constraints__, limit="constrained to")
            bound = type_parameter.__bound__
            if bound is None or bound is object:
                return {"kind": "any"}, "value"
            if _is_a_union(bound):
                return self._one_of(typing.get_args(bound), limit="bound to")
            pin_type = self._a_member_pin_type(bound, limit="bound to")
            return {"kind": "is", "pin_type": pin_type}, the_flow_a_function_input_reads(pin_type)
        if written_type is object:
            return {"kind": "any"}, "value"
        return self._one_of(typing.get_args(written_type))

    def _one_of(
        self, members: "Sequence[object]", *, limit: Optional[TypeParameterLimit] = None
    ) -> "tuple[DeclaredPinType, PinFlow]":
        pin_types: list[PinType] = []
        for member in members:
            pin_type = self._a_member_pin_type(member, limit=limit)
            if pin_type in pin_types:
                raise self._refusal(
                    f"{self._pin_named} is typed one of several types, and names "
                    f"{_name_of(member)} twice. To fix: name each type once.",
                    "pin_annotation_repeated",
                )
            pin_types.append(pin_type)
        if self._form != "pin":
            return {"kind": "one_of", "pin_types": pin_types}, the_flow_a_function_input_reads(pin_types[0])
        not_streamed = [each for each in pin_types if each["type"] not in PIN_TYPES_A_PIN_OF_SEVERAL_TYPES_TAKES]
        if not_streamed:
            raise self._refusal(words_refusing_a_pin_of_several_types_taking(self._pin_named, not_streamed[0]["type"]), "pin_type_not_read_yet")
        return {"kind": "one_of", "pin_types": pin_types}, "stream"

    def _a_union_input_pin(self, written_type: object) -> bool:
        """Whether the pin is a class node's `InputPin` of several types."""
        return self._form == "pin" and self._direction == "input" and _is_a_union(written_type)

    def _a_member_pin_type(
        self, member: object, *, limit: Optional[TypeParameterLimit] = None
    ) -> PinType:
        """The pin type one type of a union or a type parameter's bound names.

        A type parameter's bound may name a type only a pin carries: its value is handed on
        unread, so the body never holds it as a value.
        """
        member_type, metadata, optional = self._written_type_metadata_and_optional(member)
        if optional or member_type is object or isinstance(member_type, typing.TypeVar) or (
            member_type is typing.Any
        ):
            raise self._refusal(
                f"{self._pin_named} is typed one of several types, and {_name_of(member)} is "
                f"not one type. To fix: name each type it takes, `float | int`.",
                "pin_type_unknown",
            )
        for entry in metadata:
            if not isinstance(entry, (ExactNumberSize, PinTypeNarrowing)):
                raise self._refusal(
                    f"{self._pin_named} is typed one of several types, and {_name_of(member)} "
                    f"carries {entry!r}, which only a pin of one type takes. To fix: drop it.",
                    "pin_annotation_not_taken",
                )
        pin_type = self._pin_type_of(member_type, self._exact_size_among(metadata), limit=limit)
        try:
            return pin_type_narrowed_by(metadata, pin_type, self._pin_named)
        except PinAnnotationNotTaken as not_taken:
            raise self._refusal(not_taken.words, not_taken.finding_code) from None

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
                if optional:
                    break
                the_rest = tuple(member for member in members if member is not type(None))
                written_type = the_rest[0] if len(the_rest) == 1 else typing.Union[the_rest]  # noqa: UP007
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
        limit: Optional[TypeParameterLimit] = None,
    ) -> PinType:
        written_form = self._written_form_of(written_type, exact_size)
        spelling = pin_type_spelling_written_as(written_form)
        row = _PIN_TYPE_DESCRIPTION_BY_NAME[spelling.pin_type_name]
        if self._member_of is not None:
            refuse_a_member_type_a_bundle_does_not_hold(
                self._pin_named,
                self._member_of,
                written_type,
                spelling,
                row,
                as_an_element=as_an_element,
                of_a_list_element=self._of_a_list_element,
                refusal=self._refusal,  # pyright: ignore[reportArgumentType]
            )
        if spelling.only_a_pin_carries and limit is not None and "value" not in row.flows:
            raise self._refusal(
                words_refusing_a_type_parameter_limited_to(self._pin_named, self._pin_name, row, limit),
                "pin_flow_not_taken",
            )
        a_member_carried_by_reference = (
            self._member_of is not None
            and not as_an_element
            and spelling.pin_type_name in HEAVY_MEMBER_TYPES_A_BUNDLE_CARRIES
        )
        if (
            spelling.only_a_pin_carries
            and limit is None
            and (self._form != "pin" or as_an_element)
            and not a_member_carried_by_reference
        ):
            raise self._refusal_of_a_type_only_a_pin_carries(row, as_an_element=as_an_element)
        if spelling.not_read_yet_because is not None and not self._reads_a_type_choice(written_form):
            raise self._refusal(
                f"{self._pin_named} is typed `{written_form}`, the {row.label} pin type, which "
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
        if written_form == "dataclass":
            bundle = bundle_of_the_fields_of(
                written_type,  # pyright: ignore[reportArgumentType]
                pin_named=self._pin_named,
                refusal=self._refusal,  # pyright: ignore[reportArgumentType]
                member_of_a_field=lambda field, annotation: self._member_of_a_field(
                    field, annotation, as_an_element=as_an_element
                ),
            )
            if self._form != "pin" and holds_a_member_carried_by_reference(bundle):
                raise self._refusal(
                    words_refusing_a_bundle_only_a_pin_carries(
                        self._pin_named, self._pin_name, self._form, written_type, bundle
                    ),
                    "field_typed_as_a_stream" if self._form == "field" else "pin_type_not_read_yet",
                )
            return bundle
        module_declared = the_pin_type_of_a_module_declared_type(written_type, pin_named=self._pin_named, read_where_a_connection_goes=not as_an_element and self._member_of is None and (self._form, self._direction) in (("field", "input"), ("pin", "output")), refusal=self._refusal)  # pyright: ignore[reportArgumentType]
        if module_declared is not None:
            return module_declared  # pyright: ignore[reportReturnType]
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

    def _member_of_a_field(
        self, field: "dataclasses.Field[object]", annotation: object, *, as_an_element: bool
    ) -> BundleMember:
        """The member a bundle's dataclass field is, read as a class node's field is."""
        member_reading = _PinReading(
            self._node,
            field.name,
            "input",
            "field",
            member_of=self._pin_name if self._member_of is None else self._member_of,
            of_a_list_element=as_an_element,
        )
        declared = member_reading.read(annotation, _default_of(field), {})
        member_pin_type = declared.declaration["pin_type"]
        assert member_pin_type["kind"] == "is", "a member is of one type"
        member: BundleMember = {"name": field.name, "member_type": member_pin_type["pin_type"]}
        meaning = declared.declaration.get("annotations", {}).get("meaning")
        if meaning is not None:
            member["meaning"] = meaning
        if declared.default_value is not NO_DEFAULT:
            member["default_value"] = declared.default_value
        return member

    def _reads_a_type_choice(self, written_form: str) -> bool:
        """Whether the pin is a class node's `PinTypeChoice` field, or a `NodeReference` setting
        of the runtime's own node, the one place each is read yet."""
        return self._form == "field" and (
            written_form == "PinTypeChoice"
            or (written_form == "NodeReference" and self._of_the_runtimes_own_node)
        )

    def _refusal_of_a_type_only_a_pin_carries(
        self, row: PinTypeDescription, *, as_an_element: bool
    ) -> NodeDeclarationRefusal:
        if as_an_element:
            return self._refusal(
                f"{self._pin_named} is a List of {row.label}, which holds no stored value, and a "
                f"List element is a small type, {_written_forms_a_list_element_takes()}. To fix: "
                f"type its elements as one of them.",
                "list_element_not_taken",
            )
        if self._form == "field":
            return self._refusal(
                f"{self._pin_named} is a field typed `{row.python_forms[0]}`, a {row.label}, "
                f"which holds no stored value, so only a pin carries it. To fix: "
                f"`{self._pin_name}: InputPin[{row.python_forms[0]}]`.",
                "field_typed_as_a_stream",
            )
        return self._refusal(
            words_refusing_a_function_pin_of(
                self._pin_named, self._node, self._pin_name, self._direction, row
            ),
            "pin_type_not_read_yet",
        )

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
        if element_form not in WRITTEN_FORMS_A_LIST_ELEMENT_TAKES and element_form != "dataclass":
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
        if (
            _is_a_value_class_of_parts(written_type)
            or written_type is Asset
            or written_type in STREAM_PIN_TYPE_CLASSES
        ):
            return written_type.__name__  # pyright: ignore[reportAttributeAccessIssue]
        if (module_declared_spelling := the_spelling_of_a_module_declared_type(written_type)) is not None:
            return module_declared_spelling
        if written_type is PinTypeChoice:
            return "PinTypeChoice"
        if is_the_node_reference_class(written_type):
            return "NodeReference"
        if (
            getattr(written_type, "__name__", "") == "UnreadMessage"
            and getattr(written_type, "__module__", "").startswith("tatolab.stream")
        ):
            raise self._refusal(
                f"{self._pin_named} is typed `UnreadMessage`, and a pin under bare `@node` that "
                f"hands a value on unread is typed by a type parameter. To fix: "
                f"`def {self._node}[PinValue]({self._pin_name}: PinValue, ...) -> PinValue`.",
                "pin_type_unknown",
            )
        if isinstance(written_type, type) and dataclasses.is_dataclass(written_type):
            return "dataclass"
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
            if isinstance(entry, INPUT_DECLARATION_ANNOTATION_CLASSES):
                raise self._refusal(words_refusing_an_input_declaration_annotation_on(self._pin_named, entry), "pin_annotation_not_taken")
            annotation_name = ANNOTATION_NAME_OF_EACH_CLASS.get(type(entry))
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
                    f"pin does not take; it takes {annotation_classes_named(row)}. To fix: "
                    f"drop it.",
                    "pin_annotation_not_taken",
                )
            annotations[annotation_name] = annotation_on_the_wire(entry)
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
        if default is NO_DEFAULT and self._a_setting and self._of_the_runtimes_own_node:
            return NO_DEFAULT
        if default is NO_DEFAULT and self._member_of is not None:
            return NO_DEFAULT
        if default is NO_DEFAULT:
            raise self._refusal(
                f"{self._pin_named} has no default and takes typed values, so it would read "
                f"nothing until a value is set. To fix: give it a default of its type, or "
                f"type it `{self._python_form_of(row)} | None = None` so it may be unset.",
                "value_input_has_no_default",
            )
        try:
            return stored_form_of_a_pin_value(default, pin_type, annotations)
        except PinValueNotTaken as not_taken:
            raise self._refusal(
                f"{self._pin_named} defaults to {not_taken.value!r}, {not_taken.why}. To fix: "
                f"{fix_of_a_default_not_taken(not_taken, self._python_form_of(row))}",
                (
                    "pin_default_outside_its_range"
                    if not_taken.kind in ("outside_its_exact_size", "outside_its_range")
                    else "pin_default_not_of_its_type"
                ),
            ) from None

    def _python_form_of(self, row: PinTypeDescription) -> str:
        """How the pin's type is written: a bundle by its dataclass, any other by its row's form."""
        if row.name == "bundle" and isinstance(self._written_type, type):
            return self._written_type.__name__
        return row.python_forms[0]


def _default_of(field: "dataclasses.Field[object]") -> object:
    """A dataclass field's default, made by its factory when it has one; `NO_DEFAULT` for none."""
    if field.default is not dataclasses.MISSING:
        return field.default
    if field.default_factory is not dataclasses.MISSING:
        return field.default_factory()
    return NO_DEFAULT


def _is_a_value_class_of_parts(written_type: object) -> typing.TypeGuard[type[PinValueOfParts]]:
    return written_type in PIN_VALUE_CLASSES

