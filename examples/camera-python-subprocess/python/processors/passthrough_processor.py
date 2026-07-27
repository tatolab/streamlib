# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

from streamlib import RuntimeContextFullAccess, RuntimeContextLimitedAccess


class PassthroughProcessor:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        print("[PassthroughProcessor] setup")

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_in")
        if frame is not None:
            ctx.outputs.write("video_out", frame)

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        print("[PassthroughProcessor] teardown")
