# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The processor `graph_building_app.py` adds.

Its own module rather than the entry file because a processor class identifies
by its import path, and a class in the entry file identifies as `__main__:…` —
a name the child interpreter that hosts it cannot import.
"""

from streamlib import RuntimeContextLimitedAccess, input, output, processor


@processor
class GraphBuildingFilter:
    @input(delivery_profile="latest")
    def frames_from_upstream(self) -> None: ...

    @output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("frames_from_upstream")
        if frame is not None:
            ctx.outputs.write("frames_to_downstream", frame)
