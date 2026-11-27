# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The hub and the spokes `test_connection_wires.py` wires by a connection, straight and through a
graph variable.

In its own module because a helper process imports each class by its import path, and a class
declared inside a pytest module would have the child import the test suite.
"""

from typing import Callable
import json

from tatolab.stream import Connection, Input, OutputPin, RuntimeContextLimitedAccess, connection_type, log, node

REACHED_MARKER_PREFIX = "MARKER:CONNECTION_REACHED_"


@connection_type
class HubConnection(Connection):
    """The kind of node a hub gives out."""


@node(execution="continuous", interval_ms=50)
def Hub(
    *,
    connection: OutputPin[HubConnection],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Gives itself out on `connection`; it writes nothing, since the wire brings the node."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        del ctx

    return each_frame


@node(execution="continuous", interval_ms=20)
def Spoke(
    *,
    hub: Input[HubConnection | None] = Input(None),
    spoke: Input[str] = Input("spoke"),
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reports the node its `hub` wire brings, once, under a marker named for `spoke`."""

    reported = False

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal reported
        del ctx
        if hub.get() is None or reported:
            return
        reported = True
        log.info(f"{REACHED_MARKER_PREFIX}{spoke.get().upper()} " + json.dumps({"hub": hub.get().name}))

    return each_frame
