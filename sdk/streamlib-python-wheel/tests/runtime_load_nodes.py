# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@node` class `test_runtime_load.py` names in a graph by its type alone.

Nothing imports this module but the engine's type resolver, during
`Runtime.load` — so a graph naming `runtime_load_nodes:LoadedFrameRelay` proves
the resolver imports and registers a node type the process never imported.
"""

from tatolab.stream import RuntimeContextLimitedAccess, node


@node
class LoadedFrameRelay:
    """Passes each frame it reads downstream unchanged."""

    @node.input(delivery_profile="newest")
    def video_from_upstream(self) -> None: ...

    @node.output()
    def video_to_downstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_upstream")
        if frame is not None:
            ctx.outputs.write("video_to_downstream", frame)
