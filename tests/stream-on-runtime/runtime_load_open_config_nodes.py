# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A `@node.process` whose config takes any key, for `test_stream_graph_load_on_tatolabd.py`.

A built-in refuses a setting it does not know at load, so a test whose subject
is only what a config can carry through a load — how deep it nests — names
this node instead.
"""

from typing import Any

from tatolab.stream import Image, Input, RuntimeContextLimitedAccess, node


class OpenConfig(dict[str, Any]):
    """Any key, holding any JSON value."""


@node.process
class OpenConfigSink:
    """Reads each frame it receives, and takes any config."""

    def __init__(self, config: OpenConfig) -> None:
        self.config = config

    video_from_upstream = Input(Image, delivery="newest")

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.inputs.read("video_from_upstream")
