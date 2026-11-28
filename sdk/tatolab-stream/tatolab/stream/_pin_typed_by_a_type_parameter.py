# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""What a type parameter tells a pin it types: the one input an output typed by it follows, the
one pin type a class node's parameter is bound to, and how a function's input it types reads."""

from __future__ import annotations

import typing
from collections.abc import Callable, Mapping, Sequence
from typing import TYPE_CHECKING, Final, Optional

from ._contract_pin_types import DeclaredPinType, PinFlow, PinType
from ._description import Description
from ._formerly_named import FormerlyNamed
from ._pin_spelling_words import is_a_union, name_of
from ._pin_types import PIN_TYPE_SPELLINGS
from ._pin_value_stored_form import PIN_TYPE_DESCRIPTION_BY_NAME

if TYPE_CHECKING:
    from ._node_declaration_refusal import NodeDeclarationFindingCode

__all__ = [
    "output_pin_type_found_from_an_input",
    "the_flow_a_function_input_reads",
    "the_one_type_a_class_type_parameter_binds",
]

type Refusal = Callable[[str, "NodeDeclarationFindingCode"], Exception]
"""What makes the refusal of some words under a finding code, for the pin being read."""

# The pin types that hold no stored value, so only a pin carries them.
_PIN_TYPE_NAMES_ONLY_A_PIN_CARRIES: Final[frozenset[str]] = frozenset(
    spelling.pin_type_name for spelling in PIN_TYPE_SPELLINGS if spelling.only_a_pin_carries
)


def output_pin_type_found_from_an_input(
    *,
    pin_named: str,
    written_type: object,
    type_parameter: Optional[typing.TypeVar],
    input_ports_of_each_type_parameter: Mapping[typing.TypeVar, Sequence[str]],
    refusal: Refusal,
) -> DeclaredPinType:
    """The pin type of the output `pin_named`, the type linked into the one input its type
    parameter also types; refused when it has no type parameter or that parameter types no
    input, or several."""
    if type_parameter is None:
        raise refusal(
            f"{pin_named} is typed {name_of(written_type)}, and an output carries "
            f"one type, the one it states or the one linked into an input it hands on. To "
            f"fix: type it as one type, or by a type parameter that also types that input, "
            f"`def Gate[PinValue](value: PinValue) -> PinValue`.",
            "output_type_found_from_no_input",
        )
    input_ports = input_ports_of_each_type_parameter.get(type_parameter, ())
    if not input_ports:
        raise refusal(
            f"{pin_named} is typed `{type_parameter.__name__}`, which types no input, "
            f"so no link finds its type. To fix: type the input whose value it hands on "
            f"`{type_parameter.__name__}`.",
            "output_type_found_from_no_input",
        )
    if len(input_ports) > 1:
        raise refusal(
            f"{pin_named} is typed `{type_parameter.__name__}`, which types the inputs "
            f"{', '.join(f'`{port}`' for port in input_ports)}, and an output takes the type "
            f"linked into one of them. To fix: a type parameter for each input, typing the "
            f"output by the one it hands on.",
            "type_parameter_types_several_inputs",
        )
    return {"kind": "same_as_input", "input_port_name": input_ports[0]}


def the_one_type_a_class_type_parameter_binds(
    *,
    pin_named: str,
    node: str,
    type_parameter: typing.TypeVar,
    metadata: Sequence[object],
    optional: bool,
    refusal: Refusal,
) -> object:
    """The one type `type_parameter` is bound to, which the class node's pin `pin_named` typed by
    it carries; refused for a parameter bound to no one type, a pin that may be unset, and an
    annotation only a pin of one type takes."""
    name = type_parameter.__name__
    bound = type_parameter.__bound__
    if (
        optional
        or type_parameter.__constraints__
        or bound is None
        or bound is object
        or is_a_union(bound)
    ):
        raise refusal(
            f"{pin_named} is typed `{name}`"
            + (" | None" if optional else "")
            + f", and a node function's pin typed by a type parameter carries the one pin type "
            f"the parameter is bound to. To fix: bind it to one type, `def "
            f"{node}[{name}: Texture](...)`, and type the pin `{name}`.",
            "pin_type_not_read_yet",
        )
    for entry in metadata:
        if not isinstance(entry, (Description, FormerlyNamed)):
            raise refusal(
                f"{pin_named} is typed `{name}`, which its link finds the type of, and "
                f"carries {entry!r}, which only a pin of one type takes. To fix: drop it; "
                f"such a pin takes `FormerlyNamed` and `Description`.",
                "pin_annotation_not_taken",
            )
    return bound


def the_flow_a_function_input_reads(pin_type: PinType) -> PinFlow:
    """How a function's input of `pin_type` reads: a type only a pin carries as a Value, since a
    function reads the newest message on each input, and any other type by its first flow."""
    if pin_type["type"] in _PIN_TYPE_NAMES_ONLY_A_PIN_CARRIES:
        return "value"
    return PIN_TYPE_DESCRIPTION_BY_NAME[pin_type["type"]].flows[0]
