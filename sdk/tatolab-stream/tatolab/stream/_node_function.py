# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A node written as one function: `@node(...)` on a function, its value inputs `Input[T]`.

The function is the node's body. Its parameters are keyword-only: each value input
`level: Input[float] = Input(0.25)`, each Stream or Trigger input `video: InputPin[Texture]`,
each output `result: OutputPin[Texture]`, each pin group `layers: PinGroup[Layer]`, and the
context `ctx: RuntimeContextFullAccess`, found by its type. The body runs once, when the node
is set up, declares its computed values with `computed(build)`, and returns the function the
node calls each frame, or `None` when a computed value publishes on its own.

An input is read with `.get()` only in a build or in the per-frame function. A build's reads are
what it depends on: a set or a wire of an input rebuilds, off the frame thread, each computed
value whose build read it, and frames keep the previous value until the new one is built. A value
a build replaced, and every value one made, is closed with its `close()` once nothing holds it,
the last of them when the node stops.
"""

from __future__ import annotations

from typing import Any, Callable, Optional, Protocol, TypeVar

from ._class_node_declaration import ClassNodeMember, ClassNodePinGroup
from ._node_function_input import Input, InputReadOffItsDeclaration
from ._runtime_lend import runtime_backed_function, runtime_backed_protocol

__all__ = [
    "Computed",
    "Input",
    "InputReadOffItsDeclaration",
    "NodeFunction",
    "NodeScopeComputedValue",
    "computed",
]

_ComputedValue = TypeVar("_ComputedValue")
_ComputedValue_co = TypeVar("_ComputedValue_co", covariant=True)


@runtime_backed_protocol
class NodeScopeComputedValue(Protocol[_ComputedValue_co]):
    """A value computed from a node's inputs, read with `get()` in a build or a frame."""

    def get(self) -> _ComputedValue_co:
        """The computed value: in a build, as the build started, recorded as read; in a frame,
        always a value."""
        ...


Computed = NodeScopeComputedValue
"""A value a node written as one function computes from its inputs, which `computed` declares."""


@runtime_backed_function()
def computed(build: Callable[[], _ComputedValue]) -> NodeScopeComputedValue[_ComputedValue]:
    """Declare a value `build` computes from what it reads with `get()`, in a node's body.

    `build` takes no argument and runs off the frame thread, first once the body returns and
    again each time an input or computed value it read changes; a value equal to the last
    changes nothing. What it raises is shown on the node, and frames keep the last value it
    built. A value it replaces is closed with its `close()`, if it has one, once nothing holds
    it; an object that came in as an input's value is never closed.
    """
    ...


class NodeFunction:
    """A function under `@node(...)`: a node written as one function, added to a stream as a
    node."""

    __wrapped__: Callable[..., Any]
    __name__: str
    __qualname__: str

    members: "tuple[ClassNodeMember, ...]"
    """The value input or pin behind each port, each parameter by its name as `attribute`."""

    pin_groups: "tuple[ClassNodePinGroup, ...]"
    """Each `PinGroup` it takes, in the order of its parameters, each by its name as `attribute`."""

    full_access_context_parameter: Optional[str]
    """The parameter the runtime context is handed in; `None` for a body that takes none."""

    def __init__(
        self,
        function: Callable[..., Any],
        *,
        members: "tuple[ClassNodeMember, ...]",
        pin_groups: "tuple[ClassNodePinGroup, ...]",
        full_access_context_parameter: Optional[str],
    ) -> None:
        self.__wrapped__ = function
        self.__module__ = function.__module__
        self.__name__ = function.__name__
        self.__qualname__ = function.__qualname__
        self.__doc__ = function.__doc__
        self.members = members
        self.pin_groups = pin_groups
        self.full_access_context_parameter = full_access_context_parameter

    def __repr__(self) -> str:
        return f"<node function {self.__module__}:{self.__qualname__}>"
