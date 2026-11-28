# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A pin type a module declares by a class: a named type under `@named_pin_type`, or a connection type under `@connection_type`, read as the spelling and the pin type a declaration writes."""

from __future__ import annotations

from collections.abc import Callable
from typing import Any, Optional

from ._connection_type_declaration import connection_type_declared_by
from ._named_pin_type_declaration import named_pin_type_declared_by
from ._node_declaration_refusal import NodeDeclarationFindingCode, NodeDeclarationRefusal

__all__ = ["the_pin_type_of_a_module_declared_type", "the_spelling_of_a_module_declared_type"]


def the_spelling_of_a_module_declared_type(written_type: object) -> Optional[str]:
    """`named_pin_type` or `connection_type`, the spelling row of the class `written_type` names; `None` for any other type."""
    if named_pin_type_declared_by(written_type) is not None:
        return "named_pin_type"
    if connection_type_declared_by(written_type) is not None:
        return "connection_type"
    return None


def the_pin_type_of_a_module_declared_type(
    written_type: object,
    *,
    pin_named: str,
    read_where_a_connection_goes: bool,
    refusal: Callable[[str, NodeDeclarationFindingCode], NodeDeclarationRefusal],
) -> Optional[dict[str, Any]]:
    """The pin type the class `written_type` names declares, as a declaration writes it; `None` for any other type.

    A connection type is read only where `read_where_a_connection_goes` says the pin is a class
    node's own field or output pin, the two ends a wire of it joins; anywhere else it is refused
    through `refusal`, since nothing would hand the pin a node.
    """
    named = named_pin_type_declared_by(written_type)
    if named is not None:
        return {"type": "named", "named_pin_type": named.import_path}
    connection = connection_type_declared_by(written_type)
    if connection is None:
        return None
    if not read_where_a_connection_goes:
        written = connection.import_path.rpartition(":")[2]
        raise refusal(
            f"{pin_named} is typed `{written}`, a Connection, which a node written as one "
            f"function gives out on an output pin, `connection: OutputPin[{written}]`, and takes "
            f"on an input of its own, `hub: Input[{written} | None] = Input(None)`; a wire of it "
            f"brings a node and "
            f"carries no data, so nothing would reach it here. To fix: declare it one of those two "
            f"ways.",
            "pin_type_not_read_yet",
        )
    pin_type: dict[str, Any] = {"type": "connection", "connection_type": connection.import_path}
    if connection.one_referrer_per_referenced_node:
        pin_type["one_referrer_per_referenced_node"] = True
    return pin_type
