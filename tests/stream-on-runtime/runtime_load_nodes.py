# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@node` classes `test_stream_graph_load_on_tatolabd.py` names in a graph by type alone.

Nothing in the test process imports this module: `tatolabd` describes it in a
processor interpreter, so a graph naming `runtime_load_nodes:LoadedFrameRelay`
proves a node type registers from its description alone.
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

