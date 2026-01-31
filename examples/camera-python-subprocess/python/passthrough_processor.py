# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1


class PassthroughProcessor:
    def setup(self, ctx):
        print("[PassthroughProcessor] setup")

    def process(self, ctx):
        frame = ctx.inputs.read("video_in")
        if frame is not None:
            ctx.outputs.write("video_out", frame)

    def teardown(self, ctx):
        print("[PassthroughProcessor] teardown")
