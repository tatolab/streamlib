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
import types
import typing
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Final, Literal, Optional, final

from ._contract_pin_types import (
    PinAnnotations,
    DeclaredPinType,
    PinDeclarationOnTheWire,
    PinFlow,
    PinType,
    PinTypeDescription,
)
from ._description import Description
from ._formerly_named import FormerlyNamed
from ._input_declaration_annotations import (
    INPUT_DECLARATION_ANNOTATION_CLASSES,
    StreamDelivery,
)
from ._named_pin_type_declaration import named_pin_type_declared_by
from ._pin_annotations_on_the_wire import (
    ANNOTATION_NAME_OF_EACH_CLASS,
    annotation_classes_named,
    annotation_on_the_wire,
)
from ._pin_declaration_refusal_words import (
    TypeParameterLimit,
    words_refusing_a_function_pin_of,
    words_refusing_a_type_parameter_limited_to,
)
from ._pin_type_choice import PinTypeChoice, PinTypeFromSetting
from ._pin_typed_by_a_setting import declaration_of_a_pin_a_setting_types
from ._pin_value_stored_form import (
    PIN_TYPE_DESCRIPTION_BY_NAME as _PIN_TYPE_DESCRIPTION_BY_NAME,
    PinValueNotTaken,
    stored_form_of_a_pin_value,
)
from ._pin_types import (
    INTEGER_BOUNDS_OF_EACH_SIZE,
    INTEGER_SIZES_ONLY_AN_ELEMENT_HOLDS,
    PIN_TYPE_SPELLINGS,
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
    "outputs_not_named",
    "wire_only_input_with_a_default",
    "output_type_found_from_no_input",
    "type_parameter_types_several_inputs",
    "field_typed_as_a_stream",
    "pin_flow_not_taken",
    "attribute_not_a_pin",
    "pin_has_a_value",
    "init_takes_parameters",
    "class_takes_a_config",
    "class_declares_a_retired_port",
    "conversion_under_a_family_grammar",
    "conversion_not_one_input_to_one_output",
    "conversion_end_states_no_one_pin_type",
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
    `NO_DEFAULT` on an output, on a wire only input that may not be unset, on a class node's
    `InputPin`, which reads what its wire carries, and on a built-in's setting a graph must set."""

    type_parameter: Optional[typing.TypeVar] = None
    """The type parameter that types it, whose value its node can only hand on; `None` for a
    pin typed otherwise."""


_WRITTEN_FORMS_ONLY_AN_ELEMENT_TAKES: Final[frozenset[str]] = frozenset(
    ExactNumberSize(size).written_form for size in INTEGER_SIZES_ONLY_AN_ELEMENT_HOLDS
)

# The read types of the family grammar, by the typed spelling that replaces each.
_TYPED_SPELLING_OF_EACH_FAMILY_READ_TYPE_NAME: Final[dict[str, str]] = {
    "ImageFrame": "Texture",
    "ArrayFrame": "Tensor",
    "ChannelsBlock": "Audio",
    "PacketsUnit": "EncodedVideo` or `EncodedAudio",
}


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
    ) -> None:
        self._node = node
        self._pin_name = pin_name
        self._direction: PinDirection = direction
        self._form = form
        self._of_the_runtimes_own_node = of_the_runtimes_own_node
        self._a_setting = a_setting

    @property
    def _pin_named(self) -> str:
        return f"`{self._node}`'s {self._direction} `{self._pin_name}`"

    def _refusal(self, words: str, finding_code: NodeDeclarationFindingCode) -> NodeDeclarationRefusal:
        return NodeDeclarationRefusal(
            words, finding_code=finding_code, node=self._node, pin=self._pin_name
        )

    def read(
        self,
        annotation: object,
        default: object,
        input_ports_of_each_type_parameter: "Mapping[typing.TypeVar, Sequence[str]]",
    ) -> DeclaredPin:
        written_type, metadata, optional = self._written_type_metadata_and_optional(annotation)
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
        if (
            written_type is object
            or isinstance(written_type, typing.TypeVar)
            or _is_a_union(written_type)
        ):
            if self._form != "function":
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
        annotations = self._annotations_among(metadata, row)
        declaration: PinDeclarationOnTheWire = {
            "pin_type": {"kind": "is", "pin_type": pin_type},
            "flow": self._flow_of(row),
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
        """A pin written `Annotated[object, PinTypeFromSetting("...")]`, of the type that setting
        chooses: a value, whose default, if any, is `None`."""
        if not self._of_the_runtimes_own_node:
            raise self._refusal(
                f"{self._pin_named} carries `{type_settings[0]!r}`, which only the runtime's own "
                f"nodes declare yet. To fix: type it as one of {_spellings_read_today()}.",
                "pin_type_not_read_yet",
            )
        declaration = declaration_of_a_pin_a_setting_types(
            pin_named=self._pin_named,
            written_type=written_type,
            metadata=metadata,
            type_settings=type_settings,
            an_output=self._direction == "output",
            optional=optional,
            default=default,
            refusal=self._refusal,
        )
        self._state_former_names(declaration, metadata)
        takes_none = self._direction != "output" and optional
        return DeclaredPin(
            declaration, optional=optional, default_value=None if takes_none else NO_DEFAULT
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

    def _flow_of(self, row: PinTypeDescription) -> PinFlow:
        """How the pin reads its messages: a field's value, an `InputPin`'s stream or event, else
        its type's first flow."""
        if self._form == "function" or self._direction == "output":
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
            pin_type = self._output_pin_type_found_from_an_input(
                written_type, type_parameter, input_ports_of_each_type_parameter
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
            return {"kind": "is", "pin_type": pin_type}, _the_flow_a_function_input_reads(pin_type)
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
        return {"kind": "one_of", "pin_types": pin_types}, _the_flow_a_function_input_reads(
            pin_types[0]
        )

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
            if not isinstance(entry, ExactNumberSize):
                raise self._refusal(
                    f"{self._pin_named} is typed one of several types, and {_name_of(member)} "
                    f"carries {entry!r}, which only a pin of one type takes. To fix: drop it.",
                    "pin_annotation_not_taken",
                )
        return self._pin_type_of(
            member_type,
            self._exact_size_among(metadata),
            limit=limit,
        )

    def _output_pin_type_found_from_an_input(
        self,
        written_type: object,
        type_parameter: Optional[typing.TypeVar],
        input_ports_of_each_type_parameter: "Mapping[typing.TypeVar, Sequence[str]]",
    ) -> DeclaredPinType:
        if type_parameter is None:
            raise self._refusal(
                f"{self._pin_named} is typed {_name_of(written_type)}, and an output carries "
                f"one type, the one it states or the one linked into an input it hands on. To "
                f"fix: type it as one type, or by a type parameter that also types that input, "
                f"`def Gate[PinValue](value: PinValue) -> PinValue`.",
                "output_type_found_from_no_input",
            )
        input_ports = input_ports_of_each_type_parameter.get(type_parameter, ())
        if not input_ports:
            raise self._refusal(
                f"{self._pin_named} is typed `{type_parameter.__name__}`, which types no input, "
                f"so no link finds its type. To fix: type the input whose value it hands on "
                f"`{type_parameter.__name__}`.",
                "output_type_found_from_no_input",
            )
        if len(input_ports) > 1:
            raise self._refusal(
                f"{self._pin_named} is typed `{type_parameter.__name__}`, which types the inputs "
                f"{', '.join(f'`{port}`' for port in input_ports)}, and an output takes the type "
                f"linked into one of them. To fix: a type parameter for each input, typing the "
                f"output by the one it hands on.",
                "type_parameter_types_several_inputs",
            )
        return {"kind": "same_as_input", "input_port_name": input_ports[0]}

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
        if spelling.only_a_pin_carries and limit is not None and "value" not in row.flows:
            raise self._refusal(
                words_refusing_a_type_parameter_limited_to(self._pin_named, self._pin_name, row, limit),
                "pin_flow_not_taken",
            )
        if spelling.only_a_pin_carries and limit is None and (self._form != "pin" or as_an_element):
            raise self._refusal_of_a_type_only_a_pin_carries(row, as_an_element=as_an_element)
        if spelling.not_read_yet_because is not None and not self._reads_a_type_choice(written_form):
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
        named = named_pin_type_declared_by(written_type)
        if named is not None:
            return {"type": "named", "named_pin_type": named.import_path}  # pyright: ignore[reportReturnType]
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

    def _reads_a_type_choice(self, written_form: str) -> bool:
        """Whether the pin is a `PinTypeChoice` setting of the runtime's own node, the one place
        a type choice is read yet."""
        return (
            written_form == "PinTypeChoice"
            and self._of_the_runtimes_own_node
            and self._form == "field"
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
        if (
            _is_a_value_class_of_parts(written_type)
            or written_type is Asset
            or written_type in STREAM_PIN_TYPE_CLASSES
        ):
            return written_type.__name__  # pyright: ignore[reportAttributeAccessIssue]
        if named_pin_type_declared_by(written_type) is not None:
            return "named_pin_type"
        if written_type is PinTypeChoice:
            return "PinTypeChoice"
        typed_spelling = _TYPED_SPELLING_OF_EACH_FAMILY_READ_TYPE_NAME.get(
            getattr(written_type, "__name__", "")
        )
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
        if typed_spelling is not None and getattr(written_type, "__module__", "").startswith(
            "tatolab.stream"
        ):
            if self._form == "function":
                raise self._refusal(
                    f"{self._pin_named} is typed {_name_of(written_type)}, a read type of a "
                    f"port family, and a pin under bare `@node` is typed by its pin type, "
                    f"`{typed_spelling}`, which a function does not take yet. To fix: keep "
                    f"this node on `@node.function` until it does.",
                    "pin_type_not_read_yet",
                )
            raise self._refusal(
                f"{self._pin_named} is typed {_name_of(written_type)}, a read type of a port "
                f"family, and a class node's pin is typed by its pin type. To fix: type it "
                f"`{typed_spelling}`.",
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
                raise self._refusal_of_an_input_declaration_annotation(entry)
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

    def _refusal_of_an_input_declaration_annotation(self, entry: object) -> NodeDeclarationRefusal:
        """The refusal of `entry`, an annotation saying how a class node's input is served, on a
        pin that does not take it."""
        if isinstance(entry, StreamDelivery):
            takes_it = "a class node's Stream input takes, inside `InputPin[Annotated[T, ...]]`"
        else:
            takes_it = "a class node's field takes"
        return self._refusal(
            f"{self._pin_named} carries `{entry!r}`, which only {takes_it}. To fix: drop it, or "
            f"declare the input that way.",
            "pin_annotation_not_taken",
        )

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


# The pin types that hold no stored value, so only a pin carries them.
_PIN_TYPE_NAMES_ONLY_A_PIN_CARRIES: Final[frozenset[str]] = frozenset(
    spelling.pin_type_name for spelling in PIN_TYPE_SPELLINGS if spelling.only_a_pin_carries
)


def _written_forms_a_list_element_takes() -> str:
    return ", ".join(
        f"`{spelling.written_form}`"
        for spelling in PIN_TYPE_SPELLINGS
        if spelling.written_form in _WRITTEN_FORMS_A_LIST_ELEMENT_TAKES
    )


def _is_a_value_class_of_parts(written_type: object) -> typing.TypeGuard[type[PinValueOfParts]]:
    return written_type in PIN_VALUE_CLASSES


def _spellings_read_today() -> str:
    read_today = [
        f"`{spelling.written_form}`"
        for spelling in PIN_TYPE_SPELLINGS
        if spelling.not_read_yet_because is None
        and not spelling.only_a_pin_carries
        and spelling.written_form not in _WRITTEN_FORMS_ONLY_AN_ELEMENT_TAKES
    ]
    return ", ".join(read_today)


def _the_flow_a_function_input_reads(pin_type: PinType) -> PinFlow:
    """How a function's input of `pin_type` reads: a type only a pin carries as a Value, since a
    function reads the newest message on each input, and any other type by its first flow."""
    if pin_type["type"] in _PIN_TYPE_NAMES_ONLY_A_PIN_CARRIES:
        return "value"
    return _PIN_TYPE_DESCRIPTION_BY_NAME[pin_type["type"]].flows[0]


def _written_name_of(annotation: object) -> str:
    if isinstance(annotation, (type, typing.TypeVar)):
        return annotation.__name__
    return repr(annotation)


def _is_a_union(annotation: object) -> bool:
    return typing.get_origin(annotation) in (typing.Union, types.UnionType)


def _name_of(annotation: object) -> str:
    if isinstance(annotation, type):
        return f"`{annotation.__name__}`"
    return f"`{annotation!r}`"
