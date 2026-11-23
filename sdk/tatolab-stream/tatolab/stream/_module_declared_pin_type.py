# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A pin type a module declares by a class: a named type under `@named_pin_type`, or a connection type under `@connection_type`, read as the spelling and the pin type a declaration writes."""

from __future__ import annotations

from typing import Any, Optional

from ._connection_type_declaration import connection_type_declared_by
from ._named_pin_type_declaration import named_pin_type_declared_by

__all__ = ["the_pin_type_of_a_module_declared_type", "the_spelling_of_a_module_declared_type"]


def the_spelling_of_a_module_declared_type(written_type: object) -> Optional[str]:
    """`named_pin_type` or `connection_type`, the spelling row of the class `written_type` names; `None` for any other type."""
    if named_pin_type_declared_by(written_type) is not None:
        return "named_pin_type"
    if connection_type_declared_by(written_type) is not None:
        return "connection_type"
    return None


def the_pin_type_of_a_module_declared_type(written_type: object) -> Optional[dict[str, Any]]:
    """The pin type the class `written_type` names declares, as a declaration writes it; `None` for any other type."""
    named = named_pin_type_declared_by(written_type)
    if named is not None:
        return {"type": "named", "named_pin_type": named.import_path}
    connection = connection_type_declared_by(written_type)
    if connection is None:
        return None
    pin_type: dict[str, Any] = {"type": "connection", "connection_type": connection.import_path}
    if connection.one_referrer_per_referenced_node:
        pin_type["one_referrer_per_referenced_node"] = True
    return pin_type
