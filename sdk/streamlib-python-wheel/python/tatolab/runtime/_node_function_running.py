# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A node written as one function, as its helper runs it: the body once at setup, the function it
returned each frame.

Each value input is an input of the node's scope, holding its set value or its default, and each
pin is bound before the body runs. The body declares its computed values into the scope, which
builds them on its worker from then on; a value set or wired after is written to the scope, and
rebuilds only what read it. A frame runs once every computed value has its first value; a node
that paces itself returns no per-frame function, and its computed values publish on their own.
Stopping the node closes the scope, and with it every value a build made.
"""

from __future__ import annotations

import copy
import time
from collections.abc import Mapping
from typing import TYPE_CHECKING, Any, Callable, Optional

from tatolab.stream._node_declaration import NODE_DECLARATION_EXECUTION_STAMP
from tatolab.stream._pin_declaration import NO_DEFAULT

from tatolab.runtime._engine import NodeScopeBeingDeclared, NodeScopeInput, RunningNodeScope

if TYPE_CHECKING:
    from tatolab.stream._node_function import NodeFunction

__all__ = [
    "NodeFunctionBodyRanAgain",
    "NodeFunctionBodyReturnedNoPerFrameFunction",
    "NodeFunctionInputNotDeclared",
    "NodeFunctionPacingItselfReturnedAValue",
    "NodeFunctionRunning",
    "NodeFunctionValueInputHasNoValue",
]

#: How long a frame waits when a computed value has no first value yet, so a reactive node with a
#: message waiting is not called again at once for as long as its first build runs.
UNBUILT_FRAME_BACKOFF_SECONDS = 0.001


class NodeFunctionBodyRanAgain(RuntimeError):
    """A node function's setup was called after its body had already run."""


class NodeFunctionInputNotDeclared(ValueError):
    """A value was given, by a port's name, that names no value input of the node function."""


class NodeFunctionBodyReturnedNoPerFrameFunction(TypeError):
    """A node function's body returned something that is neither a function nor `None`."""


class NodeFunctionPacingItselfReturnedAValue(TypeError):
    """The body of a node function that paces itself (`execution="manual"`) returned something
    other than `None`, which nothing would use: such a node runs no frame."""


class NodeFunctionValueInputHasNoValue(ValueError):
    """A value input with no default was set up with no value."""


class NodeFunctionRunning:
    """One node function in its helper: its scope, its bound pins and the function its body
    returned, with the hooks the helper calls."""

    def __init__(
        self,
        node_function: "NodeFunction",
        *,
        values_to_build_with: "Mapping[str, Any]",
        pins: "Mapping[str, object]",
    ) -> None:
        self._node_function = node_function
        self._declaring: Optional[NodeScopeBeingDeclared] = NodeScopeBeingDeclared()
        self._input_of_each_port: "dict[str, NodeScopeInput]" = {}
        self._arguments: "dict[str, object]" = {}
        for member in node_function.members:
            if member.form != "field":
                self._arguments[member.attribute] = pins[member.port]
                continue
            if member.port in values_to_build_with:
                value = values_to_build_with[member.port]
            elif member.default is not NO_DEFAULT:
                value = copy.deepcopy(member.default)
            else:
                raise NodeFunctionValueInputHasNoValue(
                    f"`{node_function.__qualname__}`'s input `{member.attribute}` has no default "
                    f"and was set up with no value"
                )
            scope_input = self._declaring.input(value)
            self._input_of_each_port[member.port] = scope_input
            self._arguments[member.attribute] = scope_input
        self._running: Optional[RunningNodeScope] = None
        self._written_before_the_scope_started: "dict[str, object]" = {}
        self._per_frame: "Optional[Callable[[Any], object]]" = None

    def setup(self, ctx: Any) -> None:
        """Run the body once, with every computed value it declares declared into the node's
        scope, then start the scope: its worker builds each computed value from here on."""
        declaring = self._declaring
        if declaring is None:
            raise NodeFunctionBodyRanAgain(
                f"`{self._node_function.__qualname__}`'s body has already run, and a node "
                f"function's body runs once, at its setup"
            )
        arguments = dict(self._arguments)
        context_parameter = self._node_function.full_access_context_parameter
        if context_parameter is not None:
            arguments[context_parameter] = ctx
        body = self._node_function.__wrapped__
        returned = declaring.declare(lambda: body(**arguments))
        if returned is not None and self._paces_itself():
            raise NodeFunctionPacingItselfReturnedAValue(
                f"`{self._node_function.__qualname__}` paces itself (`execution=\"manual\"`) and "
                f"its body returned {returned!r}, which nothing would use: a node that paces "
                f"itself runs no frame, and its computed values publish on their own. To fix: "
                f"return `None`."
            )
        if returned is not None and not callable(returned):
            raise NodeFunctionBodyReturnedNoPerFrameFunction(
                f"`{self._node_function.__qualname__}`'s body returned {returned!r}, and a node "
                f"function's body returns the function the node calls each frame, or `None` "
                f"when a computed value publishes on its own"
            )
        self._per_frame = returned
        self._declaring = None
        self._running = declaring.start()
        written, self._written_before_the_scope_started = self._written_before_the_scope_started, {}
        self.write_inputs(written)

    def _paces_itself(self) -> bool:
        execution = getattr(self._node_function, NODE_DECLARATION_EXECUTION_STAMP, None)
        return isinstance(execution, dict) and execution.get("mode") == "manual"

    def process(self, ctx: Any) -> None:
        """Call the per-frame function with `ctx`, once every computed value has its first
        value, reading the values built and written since the last frame."""
        running, per_frame = self._running, self._per_frame
        if running is None or per_frame is None:
            return
        if not running.run_frame(lambda: per_frame(ctx)):
            time.sleep(UNBUILT_FRAME_BACKOFF_SECONDS)

    def stop(self, ctx: Any) -> None:
        """Close the node's scope: its worker stops once a build in flight returns, and every
        value a build made is closed."""
        self._close()

    def teardown(self, ctx: Any) -> None:
        """Close the node's scope, if `stop` did not."""
        self._close()

    def write_inputs(self, values: "Mapping[str, object]") -> None:
        """Write each of `values`, by its port's name, to the input of the node's scope it names,
        all in one write: a computed value reading several of them builds once."""
        unknown = sorted(set(values) - set(self._input_of_each_port))
        if unknown:
            raise NodeFunctionInputNotDeclared(
                f"`{self._node_function.__qualname__}` declares no value input "
                f"{', '.join(f'`{port}`' for port in unknown)}; its value inputs are "
                f"{', '.join(f'`{port}`' for port in self._input_of_each_port) or 'none'}"
            )
        running = self._running
        if running is None:
            if self._declaring is not None:
                self._written_before_the_scope_started.update(values)
            return
        running.write([(self._input_of_each_port[port], value) for port, value in values.items()])

    def _close(self) -> None:
        running, self._running, self._per_frame = self._running, None, None
        if running is not None:
            running.close()
