# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

from tatolab.stream import Image, Input, Output, RuntimeContextLimitedAccess, node


@node.meta(description="Hands on each frame it reads, unchanged.")
@node.process
class NewNode:
    video_in = Input(Image, delivery="newest", description="The frames this node reads.")
    video_out = Output(Image, description="Each frame read from `video_in`, unchanged.")

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read(self.video_in)
        if frame is None:
            return
        ctx.outputs.write(self.video_out, frame)
