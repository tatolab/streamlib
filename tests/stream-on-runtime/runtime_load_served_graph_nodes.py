# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The sink a running stream in `test_stream_graph_load_on_tatolabd.py` is read back through."""

from tatolab.stream import RuntimeContextLimitedAccess, node


@node
class LoadedFrameSink:
    """Reads each frame it receives and keeps none."""

    @node.input(delivery_profile="newest")
    def bags_from_upstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.inputs.read("bags_from_upstream")
