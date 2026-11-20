# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`NodeReference`, the reference `StreamBuilder.add` returns, recognised by its module: the
module that declares it imports the pin declaration readers, so they cannot import it."""

from __future__ import annotations

from typing import Optional

__all__ = ["is_the_node_reference_class", "node_name_a_node_reference_holds"]

_NODE_REFERENCE_MODULE = "tatolab.stream._stream_builder"


def is_the_node_reference_class(written_type: object) -> bool:
    """Whether `written_type` is the `NodeReference` class."""
    return (
        getattr(written_type, "__name__", "") == "NodeReference"
        and getattr(written_type, "__module__", "") == _NODE_REFERENCE_MODULE
    )


def node_name_a_node_reference_holds(value: object) -> Optional[str]:
    """The name of the node `value` refers to when it is a `NodeReference`; `None` for any other
    value."""
    if not is_the_node_reference_class(type(value)):
        return None
    node_name = getattr(value, "name", None)
    return node_name if isinstance(node_name, str) else None
