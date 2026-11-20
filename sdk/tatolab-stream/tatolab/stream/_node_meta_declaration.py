# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`@node.meta`: what describes a node type, declared beside the decorator that says its kind.

A kind decorator says how a node type is built and runs; `@node.meta` holds what
describes it. It is written above or below the kind decorator. Below, it runs
first and holds what it declares on the function or class until the kind
decorator takes it; above, it stamps a node already declared.
"""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any, Final, Literal, NoReturn, Optional, TypedDict

from ._description import refuse_a_description_that_is_not_a_string
from ._pin_declaration import NodeDeclarationRefusal

__all__ = [
    "PROCESS_NODE_KIND",
    "NodeDeclaredBy",
    "NodeKindDeclared",
    "NodeMeta",
    "hold_node_meta_for_the_kind_decorator",
    "read_node_meta",
    "take_node_meta_held_for_the_kind_decorator",
    "the_conversion_a_node_states",
]

NODE_META_HELD_FOR_THE_KIND_DECORATOR_ATTRIBUTE = "__tatolab_node_meta_held_for_its_kind_decorator__"

# What a category path's levels are separated by, as `Effects/Stylize`.
NODE_CATEGORY_PATH_SEPARATOR = "/"

# The kind a class node is: it runs in a process of its own.
PROCESS_NODE_KIND: Final = "process"

NodeDeclaredBy = Literal["@node", "@node.shader", "@node.process", "@node.function", "node.rust_function"]
"""The decorator that read a node's ports."""

NodeKindDeclared = Literal["process", "function", "shader"]
"""The kind a node is declared as."""

# The decorators whose pins each state a pin type, which a conversion is judged by.
_DECORATORS_DECLARING_PIN_TYPES: "tuple[NodeDeclaredBy, ...]" = ("@node", "@node.shader")

# How each count of two or more is said, up to nine.
_COUNT_OF_TWO_OR_MORE_IN_WORDS: Final = ("two", "three", "four", "five", "six", "seven", "eight", "nine")


class NodeMeta(TypedDict, total=False):
    """What one `@node.meta` declares, by key: only the keys it was given."""

    category: str
    description: str
    conversion: Literal[True]


def read_node_meta(
    *,
    category: Optional[str],
    description: Optional[str],
    conversion: Optional[Literal[True]] = None,
) -> NodeMeta:
    """The keys a `@node.meta(...)` was given, each refused unless it is what its key takes."""
    declared: NodeMeta = {}
    if category is not None:
        declared["category"] = _refuse_a_category_that_is_no_path(category)
    if description is not None:
        declared["description"] = refuse_a_description_that_is_not_a_string(
            description, "@node.meta"
        )
    if conversion is not None:
        if conversion is not True:
            raise TypeError(
                f"@node.meta takes `conversion` only as `True`, got {conversion!r}. A node that "
                f"converts one pin type to another states `conversion=True`, and a node that is "
                f"no conversion leaves it out."
            )
        declared["conversion"] = True
    if not declared:
        raise TypeError(
            "@node.meta() declares nothing; it takes `category`, `description`, `conversion` "
            'or several, `@node.meta(category="Effects/Stylize", description="…")`.'
        )
    return declared


def the_conversion_a_node_states(
    node: str,
    declared_meta: NodeMeta,
    *,
    declared_by: NodeDeclaredBy,
    kind: NodeKindDeclared,
    input_ports: "Sequence[dict[str, Any]]",
    output_ports: "Sequence[dict[str, Any]]",
) -> bool:
    """Whether `declared_meta` states that `node` is a conversion, refused unless it is one.

    `declared_by` is the decorator that read the node's ports, `kind` the kind it declared, and
    `input_ports` and `output_ports` those ports as the engine reads them. A conversion is
    declared under bare `@node` or `@node.shader`, whose pins state the types the link verdict
    reads, and has exactly one input that takes a wire and exactly one output, each stating
    exactly one pin type a wire carries; every other input is a setting.
    """
    if not declared_meta.get("conversion", False):
        return False
    if declared_by not in _DECORATORS_DECLARING_PIN_TYPES:
        raise NodeDeclarationRefusal(
            f"`{node}` states `conversion=True` under `{declared_by}`, whose ports declare "
            f"families, and a conversion is judged by the pin types its pins state. To fix: "
            f"declare it with bare `@node`, {_how_bare_node_writes_what(declared_by)}, or leave "
            f"`conversion=True` out.",
            finding_code="conversion_under_a_family_grammar",
            node=node,
        )
    wired_inputs = [port for port in input_ports if _takes_a_wire(port)]
    if not wired_inputs:
        _refuse_the_shape(node, "no input that takes a wire", "give it the one input it converts")
    if len(wired_inputs) > 1:
        _refuse_more_than_one_input_taking_a_wire(node, kind, wired_inputs)
    outputs = [port["name"] for port in output_ports]
    if not outputs:
        _refuse_the_shape(node, "no output", "give it the one output it converts to")
    if len(outputs) > 1:
        _refuse_the_shape(
            node,
            f"{_counted(outputs, 'output')}, {_listed(outputs)}",
            "keep only the output it converts to",
            pin=outputs[1],
        )
    _refuse_an_end_stating_no_one_pin_type(node, "input", wired_inputs[0])
    _refuse_an_end_stating_no_one_pin_type(node, "output", output_ports[0])
    return True


# The pin types a wire never carries, which the engine refuses on a conversion's end.
_PIN_TYPES_TAKING_NO_WIRE: Final = ("node_reference", "pin_type_choice")


def _refuse_an_end_stating_no_one_pin_type(
    node: str, direction: Literal["input", "output"], port: "dict[str, Any]"
) -> None:
    """Refuse `node` when its wired `port` states no one pin type a wire carries.

    The engine reads a conversion's two ends as exactly one pin type each, and refuses any
    other end when the stream describes the node type; this names it at decoration instead.
    A shader's port states no declaration, and its Image family is that one type.
    """
    if "pin_declaration" not in port:
        return
    declared = port["pin_declaration"]["pin_type"]
    name = port["name"]
    if declared["kind"] == "is":
        pin_type = declared["pin_type"]["type"]
        if pin_type not in _PIN_TYPES_TAKING_NO_WIRE:
            return
        takes = f"a {pin_type}, which no wire carries"
    elif declared["kind"] == "any":
        takes = "any type"
    elif declared["kind"] == "one_of":
        takes = "one of several types"
    elif declared["kind"] == "same_as_input":
        takes = f"the type of `{declared['input_port_name']}`'s link"
    else:
        takes = f"the type `{declared['input_name']}` chooses"
    raise NodeDeclarationRefusal(
        f"`{node}` states `conversion=True`, and its {direction} `{name}` takes {takes}, and a "
        f"conversion is judged by the one pin type each end states. To fix: type `{name}` by "
        f"exactly one pin type, or leave `conversion=True` out.",
        finding_code="conversion_end_states_no_one_pin_type",
        node=node,
        pin=name,
    )


def _refuse_more_than_one_input_taking_a_wire(
    node: str, kind: NodeKindDeclared, wired_inputs: "Sequence[dict[str, Any]]"
) -> NoReturn:
    """Refuse `node`, of `kind`, for the two or more `wired_inputs` it has, naming the fix.

    A class node's field takes a wire until it is marked `SettingOnly()`: its streams are
    named first and its fields after them, and the fields are the ones to mark, whatever
    order they are declared in. Every input of a function or shader node takes a wire.
    """
    if kind == PROCESS_NODE_KIND:
        fields = [port["name"] for port in wired_inputs if "pin_default" in port]
        streams = [port["name"] for port in wired_inputs if "pin_default" not in port]
        named = [*streams, *fields]
        if len(streams) > 1:
            to_fix = "keep one input that takes a wire, a field marked `SettingOnly()` for each setting"
            pin = streams[1]
        else:
            to_mark = fields if streams else fields[1:]
            to_fix = f"mark {_listed(to_mark)} as `SettingOnly()`"
            pin = to_mark[0]
    else:
        named = [port["name"] for port in wired_inputs]
        to_fix = (
            f"drop {_listed(named[1:])}, as every input of a {kind} node takes a wire and a "
            f"setting is a class node's field marked `SettingOnly()`"
        )
        pin = named[1]
    _refuse_the_shape(
        node,
        f"{_counted(named, 'input')} that take a wire, {_listed(named)}",
        to_fix,
        pin=pin,
    )


def _refuse_the_shape(node: str, has: str, to_fix: str, *, pin: Optional[str] = None) -> NoReturn:
    raise NodeDeclarationRefusal(
        f"`{node}` states `conversion=True` and has {has}. To fix: {to_fix}, or leave "
        f"`conversion=True` out.",
        finding_code="conversion_not_one_input_to_one_output",
        node=node,
        pin=pin,
    )


def _takes_a_wire(input_port: "dict[str, Any]") -> bool:
    return input_port.get("input_mode", {}).get("mode") != "setting_only"


def _how_bare_node_writes_what(declared_by: NodeDeclaredBy) -> str:
    if declared_by == "@node.process":
        return (
            "its streams as `InputPin[...]` and `OutputPin[...]` and its settings as fields "
            "marked `SettingOnly()`"
        )
    return "each pin typed by its pin type"


def _counted(names: "Sequence[str]", what: Literal["input", "output"]) -> str:
    """Two or more `names`, counted as `what`s."""
    count = len(names)
    in_words = (
        _COUNT_OF_TWO_OR_MORE_IN_WORDS[count - 2]
        if count - 2 < len(_COUNT_OF_TWO_OR_MORE_IN_WORDS)
        else str(count)
    )
    return f"{in_words} {what}s"


def _listed(names: "Sequence[str]") -> str:
    quoted = [f"`{name}`" for name in names]
    if len(quoted) <= 2:
        return " and ".join(quoted)
    return f"{', '.join(quoted[:-1])} and {quoted[-1]}"


def _refuse_a_category_that_is_no_path(category: object) -> str:
    if not isinstance(category, str):
        raise TypeError(
            f"@node.meta takes its category as a str, got {category!r}. A category is "
            f'the group an editor files the node type under, `"Effects"`, or a path '
            f'into a group inside one, `"Effects/Stylize"`.'
        )
    if any(not level.strip() for level in category.split(NODE_CATEGORY_PATH_SEPARATOR)):
        raise ValueError(
            f"@node.meta category {category!r} names a group with no name. A category is "
            f'`"Effects"` or a path whose every level is named, `"Effects/Stylize"`.'
        )
    return category


def hold_node_meta_for_the_kind_decorator(node_under_declaration: object, declared: NodeMeta) -> None:
    """Keep what a `@node.meta` under a kind decorator declares where that decorator takes it."""
    held = take_node_meta_held_for_the_kind_decorator(node_under_declaration)
    setattr(
        node_under_declaration,
        NODE_META_HELD_FOR_THE_KIND_DECORATOR_ATTRIBUTE,
        {**held, **declared},
    )


def take_node_meta_held_for_the_kind_decorator(node_under_declaration: object) -> NodeMeta:
    """What `@node.meta` declared under the kind decorator now declaring this node, taken off it.

    Read from the function's or class's own attributes, so a class never takes what a
    class it extends declared.
    """
    held = vars(node_under_declaration).get(NODE_META_HELD_FOR_THE_KIND_DECORATOR_ATTRIBUTE)
    if held is None:
        return {}
    delattr(node_under_declaration, NODE_META_HELD_FOR_THE_KIND_DECORATOR_ATTRIBUTE)
    return NodeMeta(**held)
