# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A module's connection type: a class under `@connection_type`, naming a kind of node another node takes on a wire.

A node gives itself out on an output of the type, `connection: OutputPin[MoqConnection]`, and
another takes it on a field of the type, `gateway: MoqConnection | None = None`. The wire
carries no data: on a running node the field holds an instance of the class naming the node its
wire comes from, `gateway.name`, or `None` while no wire reaches it, so the class subclasses
`Connection`. The type's name is the class's import path,
`module:QualifiedClassName`, and an end of it links only to an end of the same name.
`SoleReferrer[MoqTrack]` types a field that one node at most of its node type may take a
given node on.
"""

from __future__ import annotations

import importlib
import inspect
import typing
from dataclasses import dataclass
from typing import Any, Final, Optional, TypeVar

__all__ = [
    "CONNECTION_TYPE_STAMP",
    "Connection",
    "ConnectionTypeDeclared",
    "SoleReferrer",
    "connection_type",
    "connection_type_declared_by",
    "the_connection_to",
]

CONNECTION_TYPE_STAMP: Final = "__tatolab_connection_type__"
"""The attribute `@connection_type` stamps on the class it declares, holding a `ConnectionTypeDeclared`."""

type SoleReferrer[ConnectionTypeTheFieldTakes] = ConnectionTypeTheFieldTakes
"""A connection field one node at most of its node type may take a given node on:
`track: SoleReferrer[MoqTrack] | None = None`."""


class Connection:
    """The node a connection field holds: the class `@connection_type` declares subclasses it,
    and a running node's field holds one naming the node its wire comes from."""

    __slots__ = ("name",)

    name: str
    """The name of the node the wire comes from, as the graph names it."""

    def __init__(self, name: str) -> None:
        self.name = name

    def __eq__(self, other: object) -> bool:
        return type(other) is type(self) and typing.cast(Connection, other).name == self.name

    def __hash__(self) -> int:
        return hash((type(self), self.name))

    def __repr__(self) -> str:
        return f"{type(self).__name__}({self.name!r})"


@dataclass(frozen=True, slots=True)
class ConnectionTypeDeclared:
    """What `@connection_type` read of the class it declares, and how a pin takes it."""

    import_path: str
    """The type's name: the class's import path, `module:QualifiedClassName`."""

    one_referrer_per_referenced_node: bool = False
    """Whether `SoleReferrer[...]` wrapped it, so one node at most of the node type may take a
    given node."""


_ConnectionTypeClass = TypeVar("_ConnectionTypeClass", bound=type)


def connection_type(declared_class: _ConnectionTypeClass) -> _ConnectionTypeClass:
    """Declare the class it decorates as a connection type of its module, named by its import path.

    The class is defined at the top level of its module or inside another class, so its import
    path finds it; anything else is refused with a `TypeError` naming the class and the fix. It
    holds no data: a wire of it brings a node, never a value.
    """
    if not inspect.isclass(typing.cast(object, declared_class)):
        raise TypeError(
            f"@connection_type goes on a class, and was put on {declared_class!r}. To fix: "
            f"decorate the class that names the kind of node the wires bring."
        )
    if not issubclass(declared_class, Connection):
        raise TypeError(
            f"@connection_type is put on `{declared_class.__qualname__}`, which does not subclass "
            f"`Connection`, so a field of it could not hold the node its wire brings. To fix: "
            f"`class {declared_class.__name__}(Connection)`."
        )
    setattr(
        declared_class,
        CONNECTION_TYPE_STAMP,
        ConnectionTypeDeclared(import_path=_the_import_path_of(declared_class)),
    )
    return declared_class


def connection_type_declared_by(annotation: object) -> Optional[ConnectionTypeDeclared]:
    """What `@connection_type` declared of `annotation`, or of the class `SoleReferrer[...]` wraps; `None` for anything else.

    A subclass of a declared class is not itself declared: it has an import path of its own and
    would carry another type's name.
    """
    if typing.get_origin(annotation) is SoleReferrer:
        (wrapped,) = typing.get_args(annotation)
        declared = connection_type_declared_by(wrapped)
        if declared is None or declared.one_referrer_per_referenced_node:
            return None
        return ConnectionTypeDeclared(
            import_path=declared.import_path, one_referrer_per_referenced_node=True
        )
    if not inspect.isclass(annotation):
        return None
    declared_on_the_class: Any = annotation.__dict__.get(CONNECTION_TYPE_STAMP)
    return declared_on_the_class if isinstance(declared_on_the_class, ConnectionTypeDeclared) else None


def _the_import_path_of(declared_class: type) -> str:
    qualified_name = declared_class.__qualname__
    if "<locals>" in qualified_name.split("."):
        raise TypeError(
            f"@connection_type is put on `{qualified_name}`, which is defined inside a function, "
            f"so no import path finds it and no other node can name its type. To fix: define "
            f"the class at the top level of its module."
        )
    return f"{declared_class.__module__}:{qualified_name}"


def the_connection_to(import_path: str, node_name: str) -> Connection:
    """The value a connection field of the type `import_path` names holds for the node `node_name`.

    Refused with a `ValueError` naming the path when it finds no `@connection_type` class.
    """
    module_name, _, qualified_name = import_path.partition(":")
    try:
        found: Any = importlib.import_module(module_name)
        for attribute in qualified_name.split("."):
            found = getattr(found, attribute)
    except (ImportError, AttributeError, ValueError) as not_found:
        raise ValueError(
            f"the connection type `{import_path}` names no class its import path finds: {not_found}"
        ) from not_found
    if connection_type_declared_by(found) is None:
        raise ValueError(
            f"the connection type `{import_path}` names {found!r}, which is no `@connection_type` class"
        )
    return found(node_name)
