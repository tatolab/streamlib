# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The many-input processor `test_inbound_link_naming.py` drives, its feeders and its reporter.

In its own module because a helper process imports each class by its import
path, and a class declared inside a pytest module would have the child import
the test suite.
"""

import dataclasses
import json

from tatolab.stream import (
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    log,
    node,
)


@node
class ReportsWhichLinkEachBagCameFrom:
    """One input port, any number of producers into it.

    The shape every many-track sink has: it never declares a port per producer,
    it reads the one port and asks each bag which link it arrived on.
    """

    @node.input(delivery_profile="ordered")
    def tracks(self) -> None: ...

    @node.output()
    def attributions_to_downstream(self) -> None: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        # Links are wired before setup() runs, so a sink knows here — before a
        # single bag has arrived — how many producers it owes.
        self.links_at_setup = sorted(ctx.inputs.inbound_link_names("tracks"))

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        read = ctx.inputs.read_from_inbound_link("tracks")
        if read is None:
            return
        bag, inbound_link = read
        ctx.outputs.write(
            "attributions_to_downstream",
            {
                "value": bag["value"],
                "arrived_on": inbound_link,
                "links_at_setup": self.links_at_setup,
            },
        )


@dataclasses.dataclass
class FeedsOneValueSourceConfig:
    value: str = "unset"


@node(execution="continuous", interval_ms=100)
class FeedsOneValueSource:
    """Writes a bag carrying its configured value, again every tick.

    Again rather than once: a bag written before the far end of its link is
    listening would be lost, and the attribution asserted on is the same for
    every bag of one producer.
    """

    def __init__(self, config: FeedsOneValueSourceConfig) -> None:
        self.value = config.value

    @node.output()
    def bags_to_downstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.outputs.write("bags_to_downstream", {"value": self.value})


@node
class ReportsEachAttributionSink:
    """Reports every attribution it reads as `MARKER:ATTRIBUTION <json>`."""

    @node.input(delivery_profile="ordered")
    def attributions_from_upstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        attribution = ctx.inputs.read("attributions_from_upstream")
        if attribution is not None:
            log.info("MARKER:ATTRIBUTION " + json.dumps(dict(attribution)))
