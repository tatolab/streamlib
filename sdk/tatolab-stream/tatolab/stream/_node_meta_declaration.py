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
from typing import Any, Final, Literal, Optional, TypedDict

from ._description import refuse_a_description_that_is_not_a_string
from ._pin_declaration import NodeDeclarationRefusal

__all__ = [
    "NodeMeta",
    "hold_node_meta_for_the_kind_decorator",
    "read_node_meta",
    "take_node_meta_held_for_the_kind_decorator",
    "the_conversion_a_node_states",
]

NODE_META_HELD_FOR_THE_KIND_DECORATOR_ATTRIBUTE = "__tatolab_node_meta_held_for_its_kind_decorator__"

# What a category path's levels are separated by, as `Effects/Stylize`.
NODE_CATEGORY_PATH_SEPARATOR = "/"

# The decorators whose pins each state a pin type, which a conversion is judged by.
_DECORATORS_DECLARING_PIN_TYPES: Final = ("@node", "@node.shader")

# How each counted thing is said, from none to nine.
_COUNT_IN_WORDS: Final = ("no", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine")


class NodeMeta(TypedDict, total=False):
    """What one `@node.meta` declares, by key: only the keys it was given."""

    category: str
    description: str
    conversion: bool


def read_node_meta(
    *, category: Optional[str], description: Optional[str], conversion: Optional[bool] = None
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
    declared_by: str,
    kind: str,
    input_ports: "Sequence[dict[str, Any]]",
    output_ports: "Sequence[dict[str, Any]]",
) -> bool:
    """Whether `declared_meta` states that `node` is a conversion, refused unless it is one.

    `declared_by` is the decorator that read the node's ports, `kind` the kind it declared, and
    `input_ports` and `output_ports` those ports as the engine reads them. A conversion is
    declared under bare `@node` or `@node.shader`, whose pins state the types the link verdict
    reads, and has exactly one input that takes a wire and exactly one output; every other
    input is a setting.
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
    wired_inputs = [port["name"] for port in input_ports if _takes_a_wire(port)]
    if len(wired_inputs) != 1:
        raise NodeDeclarationRefusal(
            f"`{node}` states `conversion=True` and has "
            + (
                "no input that takes a wire. To fix: give it the one input it converts"
                if not wired_inputs
                else f"{_counted(wired_inputs, 'input')} that take a wire, "
                f"{_listed(wired_inputs)}. To fix: "
                + _how_to_take_no_wire_on(wired_inputs[1:], kind, input_ports)
            )
            + ", or leave `conversion=True` out.",
            finding_code="conversion_not_one_input_to_one_output",
            node=node,
            pin=wired_inputs[1] if wired_inputs else None,
        )
    outputs = [port["name"] for port in output_ports]
    if len(outputs) != 1:
        raise NodeDeclarationRefusal(
            f"`{node}` states `conversion=True` and has "
            + (
                "no output. To fix: give it the one output it converts to"
                if not outputs
                else f"{_counted(outputs, 'output')}, {_listed(outputs)}. To fix: keep only the "
                f"output it converts to"
            )
            + ", or leave `conversion=True` out.",
            finding_code="conversion_not_one_input_to_one_output",
            node=node,
            pin=outputs[1] if outputs else None,
        )
    return True


def _takes_a_wire(input_port: "dict[str, Any]") -> bool:
    return input_port.get("input_mode", {}).get("mode") != "setting_only"


def _how_bare_node_writes_what(declared_by: str) -> str:
    if declared_by == "@node.process":
        return (
            "its streams as `InputPin[...]` and `OutputPin[...]` and its settings as fields "
            "marked `SettingOnly()`"
        )
    return "each pin typed by its pin type"


def _how_to_take_no_wire_on(
    extra_inputs: "Sequence[str]", kind: str, input_ports: "Sequence[dict[str, Any]]"
) -> str:
    """How the inputs `extra_inputs` of a node of `kind` stop taking a wire."""
    if kind == "process":
        fields = {port["name"] for port in input_ports if "pin_default" in port}
        if all(name in fields for name in extra_inputs):
            return f"mark {_listed(extra_inputs)} as `SettingOnly()`"
        return (
            "keep one input that takes a wire, a field marked `SettingOnly()` for each setting"
        )
    return (
        f"drop {_listed(extra_inputs)}, as every input of a {kind} node takes a wire and a "
        f"setting is a class node's field marked `SettingOnly()`"
    )


def _counted(names: "Sequence[str]", what: Literal["input", "output"]) -> str:
    count = len(names)
    in_words = _COUNT_IN_WORDS[count] if count < len(_COUNT_IN_WORDS) else str(count)
    return f"{in_words} {what}{'' if count == 1 else 's'}"


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
