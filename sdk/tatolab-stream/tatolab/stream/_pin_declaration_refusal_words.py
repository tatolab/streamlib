# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The words refusing a type only a pin carries where a function under bare `@node` types a pin by it.

Such a type holds no stored value. A function takes one that flows as a Value only through a
type parameter bound to it, whose value its body hands on unread, and one that flows only as a
stream or an event not at all, since a function reads the newest message on each input.
"""

from __future__ import annotations

from typing import Literal

from ._contract_pin_types import PinTypeDescription

__all__ = [
    "TypeParameterLimit",
    "words_refusing_a_function_pin_of",
    "words_refusing_a_type_parameter_limited_to",
]

type TypeParameterLimit = Literal["bound to", "constrained to"]
"""How a type parameter limits the types it takes: by its bound, or by its constraints."""


def words_refusing_a_function_pin_of(
    pin_named: str,
    node: str,
    pin_name: str,
    direction: Literal["input", "output"],
    row: PinTypeDescription,
) -> str:
    """Why the function's pin `pin_name`, typed `row`'s type directly, is refused, with its fix."""
    written_form = row.python_forms[0]
    typed = f"{pin_named} is typed `{written_form}`, {_with_its_article(row.label)}"
    class_node = (
        f"declare a class node with "
        f"`{pin_name}: {'InputPin' if direction == 'input' else 'OutputPin'}[{written_form}]`"
    )
    if "value" not in row.flows:
        return (
            f"{typed}, which a function under bare `@node` does not take: it holds no stored "
            f"value, so only a class node's pin carries it. To fix: keep this node on "
            f"`@node.function`, or {class_node}."
        )
    if direction == "input":
        return (
            f"{typed}, which holds no stored value, so a function under bare `@node` takes it "
            f"only through a type parameter bound to it, whose value its body hands on unread. "
            f"To fix: `def {node}[Carried: {written_form}]({pin_name}: Carried, ...) -> "
            f"Carried`, keep this node on `@node.function`, or {class_node}."
        )
    return (
        f"{typed}, which holds no stored value, so a function's output carries it only as the "
        f"type linked into the input it hands on. To fix: type that input and this output by "
        f"one type parameter bound to it, `[Carried: {written_form}]`, keep this node on "
        f"`@node.function`, or {class_node}."
    )


def words_refusing_a_type_parameter_limited_to(
    pin_named: str, pin_name: str, row: PinTypeDescription, limit: TypeParameterLimit
) -> str:
    """Why a function's pin typed by a type parameter `limit` `row`'s type, which flows as no
    Value, is refused, with its fix."""
    written_form = row.python_forms[0]
    flows = " or ".join(f"a {flow}" for flow in row.flows)
    return (
        f"{pin_named} is typed by a type parameter {limit} `{written_form}`, "
        f"{_with_its_article(row.label)}, which flows only as {flows}, and a function reads the "
        f"newest message on each input, so it would drop the rest. To fix: declare a class "
        f"node with `{pin_name}: InputPin[{written_form}]`."
    )


def _with_its_article(label: str) -> str:
    return f"{'an' if label[:1].lower() in 'aeiou' else 'a'} {label}"
