# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The base every generated built-in node class derives from."""

from __future__ import annotations

import inspect
from collections.abc import Mapping
from typing import TYPE_CHECKING, Any, ClassVar, NoReturn

from ._class_node_declaration import read_class_node_ports
from ._node_declaration import NODE_DECLARATION_INPUT_PORTS_STAMP, NODE_DECLARATION_OUTPUT_PORTS_STAMP

__all__ = ["BuiltInNode", "InputValuesOfATypedBuiltInNode"]


# How an annotation names a class variable, a string under `from __future__ import annotations`.
_CLASS_VARIABLE_SPELLINGS = ("ClassVar", "typing.ClassVar")

InputValuesOfATypedBuiltInNode = Mapping[str, Any]
"""The input values a typed built-in takes, each checked against the field or pin it names."""


class BuiltInNode:
    """A node the runtime implements: passed to `stream_builder.add`, never built.

    A typed built-in declares each of its inputs and outputs as a class node does, each value
    input a field with its default and each stream a pin, so `stream_builder.add` checks the
    values it is given against them.
    """

    type: ClassVar[str]
    """The path the runtime registers this node under: the `type` a graph names."""

    def __init_subclass__(cls, **keyword_arguments: Any) -> None:
        super().__init_subclass__(**keyword_arguments)
        if not _declares_a_field_or_pin(cls):
            return
        input_ports, output_ports = read_class_node_ports(cls)
        setattr(cls, NODE_DECLARATION_INPUT_PORTS_STAMP, input_ports)
        setattr(cls, NODE_DECLARATION_OUTPUT_PORTS_STAMP, output_ports)

    if TYPE_CHECKING:
        # Uncallable, so a built-in never matches `add`'s overload for a node
        # class Python constructs, and wrong values cannot fall through to it.
        def __init__(self, *, a_built_in_node_is_never_constructed: NoReturn) -> None: ...

    else:

        def __init__(self, *_arguments: object, **_keyword_arguments: object) -> None:
            raise TypeError(
                f"`{type(self).__name__}` is a built-in node, which the runtime runs; it "
                f"is never constructed in Python. Pass the class itself to "
                f"`stream_builder.add({type(self).__name__}, input_values={{...}})`."
            )


def _declares_a_field_or_pin(built_in_node_class: type) -> bool:
    """Whether `built_in_node_class` itself annotates a field or pin: a public attribute that is
    not a `ClassVar`."""
    return any(
        not attribute_name.startswith("_") and not str(annotation).startswith(_CLASS_VARIABLE_SPELLINGS)
        for attribute_name, annotation in inspect.get_annotations(built_in_node_class).items()
    )
