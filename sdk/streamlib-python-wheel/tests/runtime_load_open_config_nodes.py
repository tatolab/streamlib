# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A `@node` whose config takes any key, for `test_runtime_load.py`.

A built-in refuses a setting it does not know at load, so a test whose subject
is only what a config can carry through `load` — how deep it nests, a NaN read
as null — names this node instead.
"""

from typing import Any

from tatolab.stream import RuntimeContextLimitedAccess, input, node


class OpenConfig(dict[str, Any]):
    """Any key, holding any JSON value."""


@node
class OpenConfigSink:
    """Reads each frame it receives, and takes any config."""

    def __init__(self, config: OpenConfig) -> None:
        self.config = config

    @input(delivery_profile="newest")
    def video_from_upstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.inputs.read("video_from_upstream")
