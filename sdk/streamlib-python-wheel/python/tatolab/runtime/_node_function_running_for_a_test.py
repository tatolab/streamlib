# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A node written as one function, run as its helper runs it, its one computed value recorded as
its body declares it so a module's test can read what it built, or why its build failed."""

from __future__ import annotations

import types
from typing import Any

from ._node_function_running import NodeFunctionRunning

#: Long enough for a device to open, or for the null backend's clock to start.
SETTLED_WITHIN_SECONDS = 5.0


class NodeFunctionRunningForATest:
    """`node_function`, its body run in `context`, its one computed value declared through
    `module`'s `computed` and recorded."""

    def __init__(
        self,
        node_function: Any,
        module: types.ModuleType,
        context: Any,
        *,
        values: "dict[str, object]",
        pins: "dict[str, object]",
    ) -> None:
        declared: "list[Any]" = []
        declare = module.computed

        def declared_and_recorded(build: Any) -> Any:
            computed_value = declare(build)
            declared.append(computed_value)
            return computed_value

        self.context = context
        self.running = NodeFunctionRunning(node_function, values_to_build_with=values, pins=pins)
        # The body declares only while it runs, in setup, so `computed` is recorded only then.
        setattr(module, "computed", declared_and_recorded)
        try:
            self.running.setup(context)
        finally:
            setattr(module, "computed", declare)
        (self._computed_value,) = declared

    def _settled_scope(self) -> Any:
        scope = self.running._running
        assert scope is not None, "the node is stopped"
        assert scope.wait_until_settled(SETTLED_WITHIN_SECONDS), "the build settles"
        return scope

    def built(self) -> Any:
        """The value the last build made, once the node's builds settle."""
        built: "list[Any]" = []
        assert self._settled_scope().run_frame(
            lambda: built.append(self._computed_value.get())
        ), "the computed value has been built"
        return built[0]

    def why_the_last_build_failed(self) -> str:
        """Why the last build failed, once the node's builds settle."""
        last_build = self._settled_scope().last_build_of(self._computed_value)
        assert last_build.state == "failed", last_build.state
        assert last_build.failure == "raised", last_build.failure
        return str(last_build.why)

    def set(self, **values: object) -> None:
        """Set `values` while the node runs, as a live set does."""
        self.running.write_inputs(values)

    def stop(self) -> None:
        self.running.stop(self.context)
