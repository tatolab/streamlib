# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The hub and the spokes `test_connection_wires.py` wires by a connection, straight and through a
graph variable.

In its own module because a helper process imports each class by its import path, and a class
declared inside a pytest module would have the child import the test suite.
"""

import json

from tatolab.stream import Connection, OutputPin, connection_type, log, node

REACHED_MARKER_PREFIX = "MARKER:CONNECTION_REACHED_"


@connection_type
class HubConnection(Connection):
    """The kind of node a hub gives out."""


@node(execution="continuous", interval_ms=50)
class Hub:
    """Gives itself out on `connection`; it writes nothing, since the wire brings the node."""

    connection: OutputPin[HubConnection]

    def process(self, ctx) -> None:
        del ctx


@node(execution="continuous", interval_ms=20)
class Spoke:
    """Reports the node its `hub` wire brings, once, under a marker named for `spoke`."""

    hub: HubConnection | None = None
    spoke: str = "spoke"

    def __init__(self) -> None:
        self._reported = False

    def process(self, ctx) -> None:
        del ctx
        if self.hub is None or self._reported:
            return
        self._reported = True
        log.info(f"{REACHED_MARKER_PREFIX}{self.spoke.upper()} " + json.dumps({"hub": self.hub.name}))
